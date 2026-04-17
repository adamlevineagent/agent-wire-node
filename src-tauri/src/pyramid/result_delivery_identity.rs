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
