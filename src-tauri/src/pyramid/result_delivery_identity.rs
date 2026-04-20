//! Result-delivery JWT verification — the Wire → Requester inbound
//! auth primitive for Phase 3.
//!
//! Direction-flipped sibling of [`super::market_identity`]. When the
//! Wire's delivery worker POSTs a result envelope to this node's
//! `/v1/compute/job-result` receiver, the authorization is a JWT
//! minted by Wire per-attempt with:
//!
//! 1. `aud = "result-delivery"` (distinct from `"compute"` so a
//!    dispatch JWT replayed at the delivery endpoint fails).
//! 2. `iss = "wire"`.
//! 3. `sub = <uuid_job_id>` — MUST match the `job_id` in the envelope
//!    body. Rejects token-for-other-job replay.
//! 4. `rid = <requester_operator_id>` — MUST match the receiving
//!    node's own operator_id. Rejects cross-operator mis-delivery
//!    even if a token leaks.
//! 5. `exp`/`iat` validated at decode time.
//!
//! Same Ed25519 signing key as the dispatch JWT — single key, many
//! audiences. No shared secret stored anywhere on node or Wire; the
//! JWT signature itself is the proof. See contract rev 1.4 §2.5 and
//! DD-W34 Option Y for the full rationale.
//!
//! Mirror table:
//!   - dispatch    (Wire → Provider):  aud="compute",         pid-check (provider_node_id)
//!   - delivery    (Wire → Requester): aud="result-delivery", rid-check (requester_operator_id)
//!
//! Both check `sub` binds to the job the caller says this is for.

use std::fmt;

use jsonwebtoken::{decode, Algorithm, DecodingKey, Validation};
use serde::Deserialize;

/// Verified result-delivery identity extracted from a valid inbound
/// delivery JWT.
///
/// Guarantees on construction:
/// - `sub_job_id` non-empty and equal to the `job_id` the caller
///   asserts the envelope is for (caller passes in the body's job_id
///   so the verifier enforces the match rather than the handler
///   double-checking).
/// - `rid` non-empty and equal to `self_operator_id`.
///
/// Private fields so the only way to construct this is via the
/// verifier — prevents tests or other callers from building an
/// identity with mismatched bindings and silently defeating the
/// non-empty contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResultDeliveryIdentity {
    sub_job_id: String,
    rid: String,
}

impl ResultDeliveryIdentity {
    /// Job id this delivery is for (JWT `sub`). Guaranteed to equal
    /// the `asserted_job_id` passed into the verifier (and non-empty).
    pub fn sub_job_id(&self) -> &str {
        &self.sub_job_id
    }

    /// Requester operator_id this delivery is addressed to (JWT
    /// `rid`). Guaranteed to equal the `self_operator_id` passed into
    /// the verifier.
    pub fn rid(&self) -> &str {
        &self.rid
    }
}

/// Failure modes for [`verify_result_delivery_token`]. Handlers map
/// every variant to 401.
#[derive(Clone, PartialEq, Eq)]
pub enum ResultDeliveryAuthError {
    /// Token failed decode: signature, audience, or expiration invalid.
    InvalidToken,
    /// `claims.sub` was absent, empty, or didn't match the envelope's
    /// `job_id`. Cross-job replay caught here.
    JobIdMismatch,
    /// `claims.rid` didn't match this node's `self_operator_id`.
    /// Cross-operator mis-delivery caught here.
    OperatorMismatch,
    /// `self_operator_id` or `asserted_job_id` was empty when passed
    /// into the verifier. A programming error on the handler side,
    /// but surfaced as auth-fail so we never silently skip the check.
    MissingVerifierInput,
}

impl fmt::Debug for ResultDeliveryAuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidToken => write!(f, "ResultDeliveryAuthError::InvalidToken"),
            Self::JobIdMismatch => write!(f, "ResultDeliveryAuthError::JobIdMismatch"),
            Self::OperatorMismatch => write!(f, "ResultDeliveryAuthError::OperatorMismatch"),
            Self::MissingVerifierInput => {
                write!(f, "ResultDeliveryAuthError::MissingVerifierInput")
            }
        }
    }
}

impl fmt::Display for ResultDeliveryAuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidToken => write!(
                f,
                "result-delivery JWT decode failed (signature, aud, or exp invalid)"
            ),
            Self::JobIdMismatch => write!(
                f,
                "result-delivery JWT sub does not match envelope job_id"
            ),
            Self::OperatorMismatch => write!(
                f,
                "result-delivery JWT rid does not match this node's operator_id"
            ),
            Self::MissingVerifierInput => write!(
                f,
                "verifier called with empty operator_id or job_id — programming error"
            ),
        }
    }
}

impl std::error::Error for ResultDeliveryAuthError {}

/// Claims shape. `aud` + `exp` enforced at decode; `iss`/`iat`
/// observability-only. `sub` and `rid` checked by this verifier.
#[derive(Debug, Deserialize)]
struct ResultDeliveryClaims {
    #[allow(dead_code)]
    aud: Option<String>,
    #[allow(dead_code)]
    iss: Option<String>,
    #[allow(dead_code)]
    exp: Option<u64>,
    #[allow(dead_code)]
    iat: Option<u64>,
    sub: Option<String>,
    rid: Option<String>,
}

/// Decode + verify a result-delivery JWT and return a typed identity.
///
/// The `asserted_job_id` is the `job_id` field from the envelope body
/// that accompanied this JWT. We require `claims.sub == asserted_job_id`
/// so a token valid for job A cannot be used to deliver a body
/// claiming to be for job B. Both values must be UUIDs (string form)
/// per contract §10.5 rev 1.3 — the dispatch body's `job_id` is UUID,
/// and the delivery JWT's `sub` mirrors it.
///
/// `self_operator_id` is this node's own operator_id from
/// `AuthState.user_id` (or equivalent). We require `claims.rid ==
/// self_operator_id` so a token addressed to operator A cannot be
/// redeemed at operator B's tunnel.
///
/// The `bearer_token` argument may include a leading `"Bearer "`
/// prefix, which is stripped before decode.
pub fn verify_result_delivery_token(
    bearer_token: &str,
    public_key: &str,
    self_operator_id: &str,
    asserted_job_id: &str,
) -> Result<ResultDeliveryIdentity, ResultDeliveryAuthError> {
    if self_operator_id.is_empty() || asserted_job_id.is_empty() {
        return Err(ResultDeliveryAuthError::MissingVerifierInput);
    }

    let token = bearer_token.strip_prefix("Bearer ").unwrap_or(bearer_token);

    let decoding_key = DecodingKey::from_ed_pem(public_key.as_bytes())
        .map_err(|_| ResultDeliveryAuthError::InvalidToken)?;

    let mut validation = Validation::new(Algorithm::EdDSA);
    validation.validate_exp = true;
    validation.set_required_spec_claims(&["exp"]);
    validation.set_audience(&["result-delivery"]);
    // Match `verify_market_identity`'s clock tolerance behavior —
    // jsonwebtoken's default leeway is 0s. Adding a 60s skew tolerance
    // matches Wire's side (per contract §3.2) and survives the short
    // (120s default) delivery TTL under realistic clock drift.
    validation.leeway = 60;

    let token_data = decode::<ResultDeliveryClaims>(token, &decoding_key, &validation)
        .map_err(|_| ResultDeliveryAuthError::InvalidToken)?;
    let claims = token_data.claims;

    let sub_job_id = claims.sub.unwrap_or_default();
    if sub_job_id.is_empty() || sub_job_id != asserted_job_id {
        return Err(ResultDeliveryAuthError::JobIdMismatch);
    }

    let rid = claims.rid.unwrap_or_default();
    if rid.is_empty() || rid != self_operator_id {
        return Err(ResultDeliveryAuthError::OperatorMismatch);
    }

    Ok(ResultDeliveryIdentity { sub_job_id, rid })
}

// ──────────────────────────────────────────────────────────────────────
// Rev 2.0 — Requester-delivery JWT verifier
//
// Sibling of `verify_result_delivery_token`. Same Ed25519 key material,
// same `iss="wire"` check, same ±60s skew, same UUID `sub`/`job_id`
// binding — but with `aud="requester-delivery"` (the rev 2.0 P2P
// audience) instead of legacy `aud="result-delivery"`.
//
// Clean-cut, no dual-aud fallback. Per spec §"Requester-delivery JWT
// verifier", rev 2.0 contract §3.4 sanctions only `aud="requester-
// delivery"`; admitting both-aud opens a small replay surface for
// outstanding legacy tokens. The legacy verifier above stays intact
// during transition for existing callers; once no reads remain it will
// be removed in a later commit.
//
// Error variants are more fine-grained than the legacy
// `ResultDeliveryAuthError` — each spec-reject reason maps to a
// distinct variant with `got`/`expected` detail. Handlers still map
// every variant to HTTP 401 per contract §3.4.
// ──────────────────────────────────────────────────────────────────────

/// Failure modes for [`verify_requester_delivery_token`]. Handlers map
/// every variant to HTTP 401 per contract §3.4.
#[derive(Clone, PartialEq, Eq)]
pub enum RequesterDeliveryVerifyError {
    /// `Authorization` header was absent, empty, or didn't parse as a
    /// bearer token. (The verifier itself accepts raw tokens too and
    /// strips a leading `"Bearer "` prefix; this variant exists so
    /// handlers can surface a distinct 401 reason when the header is
    /// structurally malformed upstream of the decode attempt.)
    MissingOrMalformedBearer,
    /// Ed25519 signature did not verify, OR the token was not a
    /// well-formed JWT, OR the public key material was malformed.
    InvalidSignature,
    /// `exp` claim was in the past (beyond the ±60s skew tolerance),
    /// OR `exp` was missing entirely.
    Expired,
    /// `aud` claim was present but not the string `"requester-delivery"`.
    /// Catches both replayed legacy tokens (`aud="result-delivery"`)
    /// and dispatch-token replay (`aud="compute"`).
    WrongAud { got: String },
    /// `iss` claim was present but not the string `"wire"`.
    WrongIss,
    /// `rid` claim did not match the receiver's `self.operator_id`.
    /// Catches cross-operator mis-delivery.
    WrongRid { got: String, expected: String },
    /// `sub` claim did not match the envelope body's `job_id`.
    /// Catches cross-job token replay.
    WrongSub { got: String, expected: String },
    /// Claims JSON was structurally invalid, or required fields (sub,
    /// rid, aud, iss, exp) were missing or empty, or sub/expected_job_id
    /// did not parse as valid UUIDs. Also covers the verifier-input
    /// guard (`expected_operator_id` or `expected_job_id` empty) —
    /// surfaced as 401 so we never silently skip the check.
    MalformedClaims,
}

impl fmt::Debug for RequesterDeliveryVerifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingOrMalformedBearer => {
                write!(f, "RequesterDeliveryVerifyError::MissingOrMalformedBearer")
            }
            Self::InvalidSignature => {
                write!(f, "RequesterDeliveryVerifyError::InvalidSignature")
            }
            Self::Expired => write!(f, "RequesterDeliveryVerifyError::Expired"),
            Self::WrongAud { got } => {
                write!(f, "RequesterDeliveryVerifyError::WrongAud {{ got: {:?} }}", got)
            }
            Self::WrongIss => write!(f, "RequesterDeliveryVerifyError::WrongIss"),
            Self::WrongRid { got, expected } => write!(
                f,
                "RequesterDeliveryVerifyError::WrongRid {{ got: {:?}, expected: {:?} }}",
                got, expected
            ),
            Self::WrongSub { got, expected } => write!(
                f,
                "RequesterDeliveryVerifyError::WrongSub {{ got: {:?}, expected: {:?} }}",
                got, expected
            ),
            Self::MalformedClaims => {
                write!(f, "RequesterDeliveryVerifyError::MalformedClaims")
            }
        }
    }
}

impl fmt::Display for RequesterDeliveryVerifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingOrMalformedBearer => write!(
                f,
                "requester-delivery Authorization header missing or malformed"
            ),
            Self::InvalidSignature => write!(
                f,
                "requester-delivery JWT signature invalid or token malformed"
            ),
            Self::Expired => write!(f, "requester-delivery JWT expired"),
            Self::WrongAud { got } => write!(
                f,
                "requester-delivery JWT aud mismatch: got {:?}, expected \"requester-delivery\"",
                got
            ),
            Self::WrongIss => write!(
                f,
                "requester-delivery JWT iss mismatch: expected \"wire\""
            ),
            Self::WrongRid { got, expected } => write!(
                f,
                "requester-delivery JWT rid mismatch: got {:?}, expected {:?}",
                got, expected
            ),
            Self::WrongSub { got, expected } => write!(
                f,
                "requester-delivery JWT sub mismatch: got {:?}, expected {:?}",
                got, expected
            ),
            Self::MalformedClaims => write!(
                f,
                "requester-delivery JWT claims malformed or verifier input empty"
            ),
        }
    }
}

impl std::error::Error for RequesterDeliveryVerifyError {}

/// Claims shape for the rev 2.0 requester-delivery JWT. All five
/// fields are required; absence is treated as malformed.
#[derive(Debug, Deserialize)]
struct RequesterDeliveryClaims {
    aud: Option<String>,
    iss: Option<String>,
    exp: Option<u64>,
    sub: Option<String>,
    rid: Option<String>,
}

/// Decode + verify a rev 2.0 requester-delivery JWT.
///
/// Returns `Ok(())` iff every spec-required check passes:
/// - Ed25519 signature against `jwt_public_key` (PEM).
/// - `aud == "requester-delivery"` (exact; no dual-aud fallback).
/// - `iss == "wire"`.
/// - `exp` not in the past (±60s skew tolerance, same as legacy).
/// - `sub` parses as a UUID and equals `expected_job_id` (which must
///   itself be a valid UUID — §10.5 uuid-not-handle-path).
/// - `rid` equals `expected_operator_id` (the receiving node's
///   `self.operator_id`).
///
/// `bearer_token` may include a leading `"Bearer "` prefix which is
/// stripped before decode.
///
/// See contract §3.4 and spec §"Requester-delivery JWT verifier" for
/// the full rationale.
pub fn verify_requester_delivery_token(
    bearer_token: &str,
    jwt_public_key: &str,
    expected_operator_id: &str,
    expected_job_id: &str,
) -> Result<(), RequesterDeliveryVerifyError> {
    if expected_operator_id.is_empty() || expected_job_id.is_empty() {
        return Err(RequesterDeliveryVerifyError::MalformedClaims);
    }
    // Require expected_job_id be a UUID per contract §10.5 — we compare
    // the JWT's sub against this value as a string, but we also want
    // the caller to be handing us well-formed UUIDs so the match below
    // is meaningful.
    if uuid::Uuid::parse_str(expected_job_id).is_err() {
        return Err(RequesterDeliveryVerifyError::MalformedClaims);
    }

    let token = match bearer_token.strip_prefix("Bearer ") {
        Some(stripped) if !stripped.is_empty() => stripped,
        Some(_) => return Err(RequesterDeliveryVerifyError::MissingOrMalformedBearer),
        None if bearer_token.is_empty() => {
            return Err(RequesterDeliveryVerifyError::MissingOrMalformedBearer)
        }
        None => bearer_token,
    };

    let decoding_key = DecodingKey::from_ed_pem(jwt_public_key.as_bytes())
        .map_err(|_| RequesterDeliveryVerifyError::InvalidSignature)?;

    // Decode WITHOUT jsonwebtoken's aud/exp validation so we can emit
    // the fine-grained error variants the contract asks for (WrongAud
    // with `got`, explicit `Expired`, etc.). We still enforce the
    // Ed25519 signature via Algorithm::EdDSA and check exp / aud / iss
    // ourselves below.
    let mut validation = Validation::new(Algorithm::EdDSA);
    validation.validate_exp = false;
    validation.validate_aud = false;
    validation.required_spec_claims.clear();
    // We still honor the same ±60s skew the legacy verifier uses.
    validation.leeway = 60;

    let token_data = match decode::<RequesterDeliveryClaims>(token, &decoding_key, &validation) {
        Ok(td) => td,
        Err(err) => {
            use jsonwebtoken::errors::ErrorKind;
            return Err(match err.kind() {
                ErrorKind::InvalidToken
                | ErrorKind::InvalidSignature
                | ErrorKind::InvalidAlgorithm
                | ErrorKind::InvalidAlgorithmName
                | ErrorKind::InvalidKeyFormat
                | ErrorKind::Crypto(_)
                | ErrorKind::Base64(_)
                | ErrorKind::Json(_)
                | ErrorKind::Utf8(_) => RequesterDeliveryVerifyError::InvalidSignature,
                _ => RequesterDeliveryVerifyError::InvalidSignature,
            });
        }
    };
    let claims = token_data.claims;

    // ── iss ─────────────────────────────────────────────────────────
    match claims.iss.as_deref() {
        Some("wire") => {}
        Some(_) => return Err(RequesterDeliveryVerifyError::WrongIss),
        None => return Err(RequesterDeliveryVerifyError::MalformedClaims),
    }

    // ── aud ─────────────────────────────────────────────────────────
    // Exact match — no dual-aud fallback. aud="compute" (dispatch
    // replay) and aud="result-delivery" (legacy) both fail here.
    match claims.aud.as_deref() {
        Some("requester-delivery") => {}
        Some(other) => {
            return Err(RequesterDeliveryVerifyError::WrongAud {
                got: other.to_string(),
            })
        }
        None => return Err(RequesterDeliveryVerifyError::MalformedClaims),
    }

    // ── exp ─────────────────────────────────────────────────────────
    let exp = claims.exp.ok_or(RequesterDeliveryVerifyError::MalformedClaims)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .map_err(|_| RequesterDeliveryVerifyError::MalformedClaims)?;
    // ±60s skew tolerance matches legacy verifier + contract §3.2.
    if exp + 60 < now {
        return Err(RequesterDeliveryVerifyError::Expired);
    }

    // ── sub / job_id binding ────────────────────────────────────────
    let sub = claims.sub.unwrap_or_default();
    if sub.is_empty() {
        return Err(RequesterDeliveryVerifyError::MalformedClaims);
    }
    // sub MUST be a UUID per contract §10.5 — reject handle-path
    // confusion early.
    if uuid::Uuid::parse_str(&sub).is_err() {
        return Err(RequesterDeliveryVerifyError::MalformedClaims);
    }
    if sub != expected_job_id {
        return Err(RequesterDeliveryVerifyError::WrongSub {
            got: sub,
            expected: expected_job_id.to_string(),
        });
    }

    // ── rid / operator binding ──────────────────────────────────────
    let rid = claims.rid.unwrap_or_default();
    if rid.is_empty() {
        return Err(RequesterDeliveryVerifyError::MalformedClaims);
    }
    if rid != expected_operator_id {
        return Err(RequesterDeliveryVerifyError::WrongRid {
            got: rid,
            expected: expected_operator_id.to_string(),
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use ed25519_dalek::pkcs8::spki::der::pem::LineEnding;
    use ed25519_dalek::pkcs8::{EncodePrivateKey, EncodePublicKey};
    use ed25519_dalek::{SigningKey, SECRET_KEY_LENGTH};
    use jsonwebtoken::{encode, EncodingKey, Header};
    use rand::RngCore;
    use serde::Serialize;

    #[derive(Debug, Serialize)]
    struct TestClaims {
        aud: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        iss: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        sub: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        rid: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        iat: Option<u64>,
        exp: u64,
    }

    struct Keypair {
        private_pem: String,
        public_pem: String,
    }

    fn generate_keypair() -> Keypair {
        let mut secret_bytes = [0u8; SECRET_KEY_LENGTH];
        rand::thread_rng().fill_bytes(&mut secret_bytes);
        let signing_key = SigningKey::from_bytes(&secret_bytes);
        let verifying_key = signing_key.verifying_key();
        let private_pem = signing_key
            .to_pkcs8_pem(LineEnding::LF)
            .expect("pkcs8 pem")
            .to_string();
        let public_pem = verifying_key
            .to_public_key_pem(LineEnding::LF)
            .expect("public pem");
        Keypair { private_pem, public_pem }
    }

    fn future_exp() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("unix epoch")
            .as_secs()
            + 120
    }

    fn past_exp() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("unix epoch")
            .as_secs()
            - 3600
    }

    fn sign_token(claims: &TestClaims, private_pem: &str) -> String {
        let header = Header::new(Algorithm::EdDSA);
        let encoding_key = EncodingKey::from_ed_pem(private_pem.as_bytes()).expect("encoding key");
        encode(&header, claims, &encoding_key).expect("sign token")
    }

    const JOB_ID: &str = "550e8400-e29b-41d4-a716-446655440000";
    const OP_ID: &str = "84213678-bd09-4d05-9cfd-7a5446659bb4";

    fn default_claims() -> TestClaims {
        TestClaims {
            aud: "result-delivery".into(),
            iss: Some("wire".into()),
            sub: Some(JOB_ID.into()),
            rid: Some(OP_ID.into()),
            iat: Some(future_exp() - 120),
            exp: future_exp(),
        }
    }

    // ── Happy path ──────────────────────────────────────────────────

    #[test]
    fn valid_token_returns_identity() {
        let kp = generate_keypair();
        let claims = default_claims();
        let token = sign_token(&claims, &kp.private_pem);
        let identity =
            verify_result_delivery_token(&token, &kp.public_pem, OP_ID, JOB_ID).expect("should verify");
        assert_eq!(identity.sub_job_id(), JOB_ID);
        assert_eq!(identity.rid(), OP_ID);
    }

    #[test]
    fn bearer_prefix_is_stripped() {
        let kp = generate_keypair();
        let token = sign_token(&default_claims(), &kp.private_pem);
        let bearer = format!("Bearer {}", token);
        verify_result_delivery_token(&bearer, &kp.public_pem, OP_ID, JOB_ID)
            .expect("should verify with Bearer prefix");
    }

    // ── Audience ────────────────────────────────────────────────────

    #[test]
    fn dispatch_audience_rejected() {
        // A dispatch-audience JWT (aud="compute") must NOT be accepted
        // by the delivery verifier. Replaying a dispatch token at the
        // result-delivery endpoint must fail cleanly.
        let kp = generate_keypair();
        let claims = TestClaims {
            aud: "compute".into(),
            ..default_claims()
        };
        let token = sign_token(&claims, &kp.private_pem);
        let err = verify_result_delivery_token(&token, &kp.public_pem, OP_ID, JOB_ID)
            .expect_err("wrong aud must reject");
        assert_eq!(err, ResultDeliveryAuthError::InvalidToken);
    }

    // ── Expiration ──────────────────────────────────────────────────

    #[test]
    fn expired_token_rejected() {
        let kp = generate_keypair();
        let claims = TestClaims {
            exp: past_exp(),
            ..default_claims()
        };
        let token = sign_token(&claims, &kp.private_pem);
        let err = verify_result_delivery_token(&token, &kp.public_pem, OP_ID, JOB_ID)
            .expect_err("expired token must reject");
        assert_eq!(err, ResultDeliveryAuthError::InvalidToken);
    }

    // ── sub / job_id binding ────────────────────────────────────────

    #[test]
    fn sub_for_different_job_rejected() {
        // A token valid for job A must not deliver an envelope claiming
        // to be for job B.
        let kp = generate_keypair();
        let token = sign_token(&default_claims(), &kp.private_pem);
        let other_job = "11111111-2222-3333-4444-555555555555";
        let err = verify_result_delivery_token(&token, &kp.public_pem, OP_ID, other_job)
            .expect_err("cross-job replay must reject");
        assert_eq!(err, ResultDeliveryAuthError::JobIdMismatch);
    }

    #[test]
    fn missing_sub_rejected() {
        let kp = generate_keypair();
        let claims = TestClaims {
            sub: None,
            ..default_claims()
        };
        let token = sign_token(&claims, &kp.private_pem);
        let err = verify_result_delivery_token(&token, &kp.public_pem, OP_ID, JOB_ID)
            .expect_err("missing sub must reject");
        assert_eq!(err, ResultDeliveryAuthError::JobIdMismatch);
    }

    #[test]
    fn empty_sub_rejected() {
        let kp = generate_keypair();
        let claims = TestClaims {
            sub: Some(String::new()),
            ..default_claims()
        };
        let token = sign_token(&claims, &kp.private_pem);
        let err = verify_result_delivery_token(&token, &kp.public_pem, OP_ID, JOB_ID)
            .expect_err("empty sub must reject");
        assert_eq!(err, ResultDeliveryAuthError::JobIdMismatch);
    }

    // ── rid / operator binding ──────────────────────────────────────

    #[test]
    fn rid_for_different_operator_rejected() {
        // A token addressed to operator A must not be redeemable at
        // operator B's tunnel — even if someone is routing traffic
        // incorrectly, the JWT binds to the intended recipient.
        let kp = generate_keypair();
        let other_op = "99999999-9999-9999-9999-999999999999";
        let claims = TestClaims {
            rid: Some(other_op.into()),
            ..default_claims()
        };
        let token = sign_token(&claims, &kp.private_pem);
        let err = verify_result_delivery_token(&token, &kp.public_pem, OP_ID, JOB_ID)
            .expect_err("cross-operator mis-delivery must reject");
        assert_eq!(err, ResultDeliveryAuthError::OperatorMismatch);
    }

    #[test]
    fn missing_rid_rejected() {
        let kp = generate_keypair();
        let claims = TestClaims {
            rid: None,
            ..default_claims()
        };
        let token = sign_token(&claims, &kp.private_pem);
        let err = verify_result_delivery_token(&token, &kp.public_pem, OP_ID, JOB_ID)
            .expect_err("missing rid must reject");
        assert_eq!(err, ResultDeliveryAuthError::OperatorMismatch);
    }

    #[test]
    fn empty_rid_rejected() {
        let kp = generate_keypair();
        let claims = TestClaims {
            rid: Some(String::new()),
            ..default_claims()
        };
        let token = sign_token(&claims, &kp.private_pem);
        let err = verify_result_delivery_token(&token, &kp.public_pem, OP_ID, JOB_ID)
            .expect_err("empty rid must reject");
        assert_eq!(err, ResultDeliveryAuthError::OperatorMismatch);
    }

    // ── Verifier-input guards ───────────────────────────────────────

    #[test]
    fn empty_self_operator_id_rejected() {
        let kp = generate_keypair();
        let token = sign_token(&default_claims(), &kp.private_pem);
        let err = verify_result_delivery_token(&token, &kp.public_pem, "", JOB_ID)
            .expect_err("empty self_operator_id must reject");
        assert_eq!(err, ResultDeliveryAuthError::MissingVerifierInput);
    }

    #[test]
    fn empty_asserted_job_id_rejected() {
        let kp = generate_keypair();
        let token = sign_token(&default_claims(), &kp.private_pem);
        let err = verify_result_delivery_token(&token, &kp.public_pem, OP_ID, "")
            .expect_err("empty asserted_job_id must reject");
        assert_eq!(err, ResultDeliveryAuthError::MissingVerifierInput);
    }

    // ── Signature + malformed inputs ────────────────────────────────

    #[test]
    fn signature_from_wrong_key_rejected() {
        let signer_kp = generate_keypair();
        let verifier_kp = generate_keypair();
        let token = sign_token(&default_claims(), &signer_kp.private_pem);
        let err = verify_result_delivery_token(&token, &verifier_kp.public_pem, OP_ID, JOB_ID)
            .expect_err("wrong-key signature must reject");
        assert_eq!(err, ResultDeliveryAuthError::InvalidToken);
    }

    #[test]
    fn malformed_token_rejected() {
        let kp = generate_keypair();
        let err = verify_result_delivery_token("not.a.jwt", &kp.public_pem, OP_ID, JOB_ID)
            .expect_err("malformed token must reject");
        assert_eq!(err, ResultDeliveryAuthError::InvalidToken);
    }

    #[test]
    fn malformed_public_key_rejected() {
        let err = verify_result_delivery_token(
            "some.token.here",
            "-----BEGIN NOT A KEY-----",
            OP_ID,
            JOB_ID,
        )
        .expect_err("malformed pubkey must reject");
        assert_eq!(err, ResultDeliveryAuthError::InvalidToken);
    }

    #[test]
    fn display_covers_all_variants() {
        for err in [
            ResultDeliveryAuthError::InvalidToken,
            ResultDeliveryAuthError::JobIdMismatch,
            ResultDeliveryAuthError::OperatorMismatch,
            ResultDeliveryAuthError::MissingVerifierInput,
        ] {
            let s = format!("{}", err);
            assert!(!s.is_empty(), "Display should produce non-empty string");
        }
    }
}

// ──────────────────────────────────────────────────────────────────────
// Rev 2.0 — Requester-delivery JWT verifier tests
// ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod requester_delivery_tests {
    use super::*;

    use ed25519_dalek::pkcs8::spki::der::pem::LineEnding;
    use ed25519_dalek::pkcs8::{EncodePrivateKey, EncodePublicKey};
    use ed25519_dalek::{SigningKey, SECRET_KEY_LENGTH};
    use jsonwebtoken::{encode, EncodingKey, Header};
    use rand::RngCore;
    use serde::Serialize;

    #[derive(Debug, Serialize)]
    struct RqClaims {
        aud: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        iss: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        sub: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        rid: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        iat: Option<u64>,
        exp: u64,
    }

    struct Keypair {
        private_pem: String,
        public_pem: String,
    }

    fn generate_keypair() -> Keypair {
        let mut secret_bytes = [0u8; SECRET_KEY_LENGTH];
        rand::thread_rng().fill_bytes(&mut secret_bytes);
        let signing_key = SigningKey::from_bytes(&secret_bytes);
        let verifying_key = signing_key.verifying_key();
        let private_pem = signing_key
            .to_pkcs8_pem(LineEnding::LF)
            .expect("pkcs8 pem")
            .to_string();
        let public_pem = verifying_key
            .to_public_key_pem(LineEnding::LF)
            .expect("public pem");
        Keypair { private_pem, public_pem }
    }

    fn future_exp() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("unix epoch")
            .as_secs()
            + 120
    }

    fn past_exp() -> u64 {
        // 1 hour in the past, well beyond the 60s skew tolerance.
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("unix epoch")
            .as_secs()
            - 3600
    }

    fn sign_token(claims: &RqClaims, private_pem: &str) -> String {
        let header = Header::new(Algorithm::EdDSA);
        let encoding_key = EncodingKey::from_ed_pem(private_pem.as_bytes()).expect("encoding key");
        encode(&header, claims, &encoding_key).expect("sign token")
    }

    const JOB_ID: &str = "550e8400-e29b-41d4-a716-446655440000";
    const OP_ID: &str = "84213678-bd09-4d05-9cfd-7a5446659bb4";

    fn default_claims() -> RqClaims {
        RqClaims {
            aud: "requester-delivery".into(),
            iss: Some("wire".into()),
            sub: Some(JOB_ID.into()),
            rid: Some(OP_ID.into()),
            iat: Some(future_exp() - 120),
            exp: future_exp(),
        }
    }

    // ── Test 1: Happy path ─────────────────────────────────────────

    #[test]
    fn requester_delivery_jwt_verifier_happy_path() {
        let kp = generate_keypair();
        let token = sign_token(&default_claims(), &kp.private_pem);
        verify_requester_delivery_token(&token, &kp.public_pem, OP_ID, JOB_ID)
            .expect("valid token with correct aud/iss/rid/sub/exp should verify");

        // Also accept a "Bearer " prefix.
        let bearer = format!("Bearer {}", token);
        verify_requester_delivery_token(&bearer, &kp.public_pem, OP_ID, JOB_ID)
            .expect("Bearer prefix should be stripped");
    }

    // ── Test 2: aud mismatch ───────────────────────────────────────

    #[test]
    fn requester_delivery_jwt_aud_mismatch_rejected() {
        let kp = generate_keypair();

        // aud="compute" (dispatch-token replay).
        let claims = RqClaims {
            aud: "compute".into(),
            ..default_claims()
        };
        let token = sign_token(&claims, &kp.private_pem);
        let err = verify_requester_delivery_token(&token, &kp.public_pem, OP_ID, JOB_ID)
            .expect_err("aud=compute must reject");
        match err {
            RequesterDeliveryVerifyError::WrongAud { got } => {
                assert_eq!(got, "compute");
            }
            other => panic!("expected WrongAud{{got:\"compute\"}}, got {:?}", other),
        }
    }

    // ── Test 3: rid mismatch ───────────────────────────────────────

    #[test]
    fn requester_delivery_jwt_rid_mismatch_rejected() {
        let kp = generate_keypair();
        let other_op = "99999999-9999-9999-9999-999999999999";
        let claims = RqClaims {
            rid: Some(other_op.into()),
            ..default_claims()
        };
        let token = sign_token(&claims, &kp.private_pem);
        let err = verify_requester_delivery_token(&token, &kp.public_pem, OP_ID, JOB_ID)
            .expect_err("wrong rid must reject");
        match err {
            RequesterDeliveryVerifyError::WrongRid { got, expected } => {
                assert_eq!(got, other_op);
                assert_eq!(expected, OP_ID);
            }
            other => panic!("expected WrongRid, got {:?}", other),
        }
    }

    // ── Test 4: sub mismatch ───────────────────────────────────────

    #[test]
    fn requester_delivery_jwt_sub_mismatch_rejected() {
        let kp = generate_keypair();
        let other_job = "11111111-2222-3333-4444-555555555555";
        let claims = RqClaims {
            sub: Some(other_job.into()),
            ..default_claims()
        };
        let token = sign_token(&claims, &kp.private_pem);
        let err = verify_requester_delivery_token(&token, &kp.public_pem, OP_ID, JOB_ID)
            .expect_err("wrong sub must reject");
        match err {
            RequesterDeliveryVerifyError::WrongSub { got, expected } => {
                assert_eq!(got, other_job);
                assert_eq!(expected, JOB_ID);
            }
            other => panic!("expected WrongSub, got {:?}", other),
        }
    }

    // ── Test 5: expired ────────────────────────────────────────────

    #[test]
    fn requester_delivery_jwt_expired_rejected() {
        let kp = generate_keypair();
        let claims = RqClaims {
            exp: past_exp(),
            ..default_claims()
        };
        let token = sign_token(&claims, &kp.private_pem);
        let err = verify_requester_delivery_token(&token, &kp.public_pem, OP_ID, JOB_ID)
            .expect_err("expired token must reject");
        assert_eq!(err, RequesterDeliveryVerifyError::Expired);
    }

    // ── Test 6: bad signature ──────────────────────────────────────

    #[test]
    fn requester_delivery_jwt_bad_signature_rejected() {
        let signer_kp = generate_keypair();
        let verifier_kp = generate_keypair();
        let token = sign_token(&default_claims(), &signer_kp.private_pem);
        let err = verify_requester_delivery_token(&token, &verifier_kp.public_pem, OP_ID, JOB_ID)
            .expect_err("wrong-key signature must reject");
        assert_eq!(err, RequesterDeliveryVerifyError::InvalidSignature);
    }

    // ── Test 7: legacy aud explicitly rejected (spec test #22) ─────

    #[test]
    fn legacy_aud_result_delivery_rejected() {
        // Per spec §"Requester-delivery JWT verifier": clean-cut, no
        // dual-aud fallback. aud="result-delivery" (the legacy rev 0.5
        // audience) MUST reject with WrongAud — not silently accepted.
        let kp = generate_keypair();
        let claims = RqClaims {
            aud: "result-delivery".into(),
            ..default_claims()
        };
        let token = sign_token(&claims, &kp.private_pem);
        let err = verify_requester_delivery_token(&token, &kp.public_pem, OP_ID, JOB_ID)
            .expect_err("legacy aud=result-delivery must reject — no dual-aud fallback");
        match err {
            RequesterDeliveryVerifyError::WrongAud { got } => {
                assert_eq!(got, "result-delivery");
            }
            other => panic!(
                "expected WrongAud{{got:\"result-delivery\"}}, got {:?}",
                other
            ),
        }
    }

    // ── Supplementary coverage (not in the 7 required) ─────────────

    #[test]
    fn missing_iss_rejected_as_malformed() {
        let kp = generate_keypair();
        let claims = RqClaims {
            iss: None,
            ..default_claims()
        };
        let token = sign_token(&claims, &kp.private_pem);
        let err = verify_requester_delivery_token(&token, &kp.public_pem, OP_ID, JOB_ID)
            .expect_err("missing iss must reject");
        assert_eq!(err, RequesterDeliveryVerifyError::MalformedClaims);
    }

    #[test]
    fn wrong_iss_rejected() {
        let kp = generate_keypair();
        let claims = RqClaims {
            iss: Some("attacker".into()),
            ..default_claims()
        };
        let token = sign_token(&claims, &kp.private_pem);
        let err = verify_requester_delivery_token(&token, &kp.public_pem, OP_ID, JOB_ID)
            .expect_err("wrong iss must reject");
        assert_eq!(err, RequesterDeliveryVerifyError::WrongIss);
    }

    #[test]
    fn empty_bearer_token_rejected() {
        let kp = generate_keypair();
        let err = verify_requester_delivery_token("", &kp.public_pem, OP_ID, JOB_ID)
            .expect_err("empty bearer must reject");
        assert_eq!(err, RequesterDeliveryVerifyError::MissingOrMalformedBearer);

        let err = verify_requester_delivery_token("Bearer ", &kp.public_pem, OP_ID, JOB_ID)
            .expect_err("bare 'Bearer ' prefix with empty body must reject");
        assert_eq!(err, RequesterDeliveryVerifyError::MissingOrMalformedBearer);
    }

    #[test]
    fn non_uuid_sub_rejected_as_malformed() {
        let kp = generate_keypair();
        let claims = RqClaims {
            sub: Some("not-a-uuid".into()),
            ..default_claims()
        };
        let token = sign_token(&claims, &kp.private_pem);
        let err = verify_requester_delivery_token(&token, &kp.public_pem, OP_ID, JOB_ID)
            .expect_err("non-UUID sub must reject");
        assert_eq!(err, RequesterDeliveryVerifyError::MalformedClaims);
    }

    #[test]
    fn empty_verifier_inputs_rejected() {
        let kp = generate_keypair();
        let token = sign_token(&default_claims(), &kp.private_pem);

        let err = verify_requester_delivery_token(&token, &kp.public_pem, "", JOB_ID)
            .expect_err("empty expected_operator_id must reject");
        assert_eq!(err, RequesterDeliveryVerifyError::MalformedClaims);

        let err = verify_requester_delivery_token(&token, &kp.public_pem, OP_ID, "")
            .expect_err("empty expected_job_id must reject");
        assert_eq!(err, RequesterDeliveryVerifyError::MalformedClaims);

        let err = verify_requester_delivery_token(&token, &kp.public_pem, OP_ID, "not-a-uuid")
            .expect_err("non-UUID expected_job_id must reject");
        assert_eq!(err, RequesterDeliveryVerifyError::MalformedClaims);
    }

    #[test]
    fn malformed_public_key_rejected() {
        let err = verify_requester_delivery_token(
            "some.token.here",
            "-----BEGIN NOT A KEY-----",
            OP_ID,
            JOB_ID,
        )
        .expect_err("malformed pubkey must reject");
        assert_eq!(err, RequesterDeliveryVerifyError::InvalidSignature);
    }

    #[test]
    fn display_and_debug_cover_all_variants() {
        let variants = [
            RequesterDeliveryVerifyError::MissingOrMalformedBearer,
            RequesterDeliveryVerifyError::InvalidSignature,
            RequesterDeliveryVerifyError::Expired,
            RequesterDeliveryVerifyError::WrongAud {
                got: "compute".into(),
            },
            RequesterDeliveryVerifyError::WrongIss,
            RequesterDeliveryVerifyError::WrongRid {
                got: "a".into(),
                expected: "b".into(),
            },
            RequesterDeliveryVerifyError::WrongSub {
                got: "c".into(),
                expected: "d".into(),
            },
            RequesterDeliveryVerifyError::MalformedClaims,
        ];
        for v in &variants {
            assert!(!format!("{}", v).is_empty());
            assert!(!format!("{:?}", v).is_empty());
        }
    }
}
