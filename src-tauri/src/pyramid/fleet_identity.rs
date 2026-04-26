//! Fleet JWT identity verification — single source of truth.
//!
//! The three fleet-authenticated handlers (`handle_fleet_dispatch`,
//! `handle_fleet_result`, `handle_fleet_announce`) must all perform the same
//! set of checks on the incoming bearer token:
//!
//! 1. Decode an Ed25519-signed JWT with `aud = "fleet"` and `validate_exp = true`.
//! 2. Require `claims.op` to match the node's own `operator_id` (both non-empty).
//! 3. Require `claims.nid` to be present and non-empty.
//!
//! Centralizing these checks here removes the "did you check op?" / "did you
//! check nid emptiness?" / "did you accidentally re-check aud?" audit classes.
//! Handlers call [`verify_fleet_identity`] once and either get a typed
//! [`FleetIdentity`] back or reject with 403.
//!
//! The Wire JWT contract uses exactly the claim names `op` and `nid` — no
//! `#[serde(alias = ...)]` aliasing is used here, to avoid cross-contamination
//! with adjacent claim shapes (e.g. `DocumentClaims` uses `sub` for a different
//! purpose). See `server.rs` for the legacy `verify_fleet_jwt` this supersedes.

use std::fmt;

use jsonwebtoken::{decode, Algorithm, DecodingKey, Validation};
use serde::Deserialize;

/// Verified fleet identity extracted from a valid JWT.
///
/// Guarantees:
/// - `nid` is non-empty (the dispatcher's raw node_id)
/// - `op` equals the verifying node's own `self_operator_id` (non-empty)
///
/// Fields are private so `verify_fleet_identity` remains the only constructor
/// — otherwise callers could build a `FleetIdentity { nid: "".into(), op:
/// "".into() }` and silently defeat the non-empty contract. Read via the
/// `nid()` and `op()` accessors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetIdentity {
    /// Dispatcher node_id (from JWT `nid` claim). Guaranteed non-empty.
    nid: String,
    /// Operator_id (from JWT `op` claim). Guaranteed equal to
    /// `self_operator_id` passed into the verifier.
    op: String,
}

impl FleetIdentity {
    /// Dispatcher node_id (from JWT `nid` claim). Guaranteed non-empty.
    pub fn nid(&self) -> &str {
        &self.nid
    }

    /// Operator_id (from JWT `op` claim). Guaranteed equal to the
    /// `self_operator_id` passed into the verifier (which is also non-empty).
    pub fn op(&self) -> &str {
        &self.op
    }
}

/// Failure modes for [`verify_fleet_identity`].
///
/// HTTP handlers convert every variant to a generic 403; the specifics stay
/// in logs / tracing metadata.
#[derive(Clone, PartialEq, Eq)]
pub enum FleetAuthError {
    /// Token failed decode: signature, audience, or expiration invalid.
    InvalidToken,
    /// `claims.op` did not equal `self_operator_id`.
    OperatorMismatch,
    /// `claims.nid` was absent or empty.
    MissingNid,
    /// `self_operator_id` passed into the verifier was empty.
    MissingOperator,
}

impl fmt::Debug for FleetAuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FleetAuthError::InvalidToken => write!(f, "FleetAuthError::InvalidToken"),
            FleetAuthError::OperatorMismatch => write!(f, "FleetAuthError::OperatorMismatch"),
            FleetAuthError::MissingNid => write!(f, "FleetAuthError::MissingNid"),
            FleetAuthError::MissingOperator => write!(f, "FleetAuthError::MissingOperator"),
        }
    }
}

impl fmt::Display for FleetAuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FleetAuthError::InvalidToken => {
                write!(
                    f,
                    "fleet JWT decode failed (signature, aud, or exp invalid)"
                )
            }
            FleetAuthError::OperatorMismatch => {
                write!(f, "fleet JWT operator does not match local operator")
            }
            FleetAuthError::MissingNid => {
                write!(f, "fleet JWT missing or empty nid claim")
            }
            FleetAuthError::MissingOperator => {
                write!(
                    f,
                    "local operator_id is empty; cannot verify fleet identity"
                )
            }
        }
    }
}

impl std::error::Error for FleetAuthError {}

/// Internal claims shape. Deliberately self-contained — not importing
/// `FleetJwtClaims` from `server.rs` keeps this module free of cross-module
/// coupling and lets later workstreams delete the legacy struct without
/// breaking this verifier's tests.
///
/// The field names `op` and `nid` are the canonical Wire JWT contract; no
/// `#[serde(alias = ...)]` intentional.
#[derive(Debug, Deserialize)]
struct FleetClaims {
    // `aud` is enforced at decode time via `Validation::set_audience`; we do
    // not re-validate in the body. Captured here so the decoder has
    // somewhere to put it.
    #[allow(dead_code)]
    aud: Option<String>,
    op: Option<String>,
    nid: Option<String>,
    // `exp` is enforced at decode time via `Validation::validate_exp`; we do
    // not re-validate in the body. Captured here so the decoder has
    // somewhere to put it.
    #[allow(dead_code)]
    exp: Option<u64>,
}

/// Decode, verify, and normalize a fleet JWT into a typed [`FleetIdentity`].
///
/// Checks performed:
/// 1. `self_operator_id` non-empty (otherwise `MissingOperator`).
/// 2. `jsonwebtoken::decode` with `Algorithm::EdDSA`, `set_audience(&["fleet"])`,
///    and `validate_exp = true`. Any decode failure → `InvalidToken`.
/// 3. `claims.op == self_operator_id` (otherwise `OperatorMismatch`).
/// 4. `claims.nid` is `Some(s)` with `!s.is_empty()` (otherwise `MissingNid`).
///
/// The `bearer_token` argument may include a leading `"Bearer "` prefix, which
/// is stripped before decode.
///
/// On success returns a [`FleetIdentity`] where both fields are non-empty and
/// `op` is guaranteed to equal `self_operator_id`.
pub fn verify_fleet_identity(
    bearer_token: &str,
    public_key: &str,
    self_operator_id: &str,
) -> Result<FleetIdentity, FleetAuthError> {
    if self_operator_id.is_empty() {
        return Err(FleetAuthError::MissingOperator);
    }

    let token = bearer_token.strip_prefix("Bearer ").unwrap_or(bearer_token);

    let decoding_key = DecodingKey::from_ed_pem(public_key.as_bytes())
        .map_err(|_| FleetAuthError::InvalidToken)?;

    let mut validation = Validation::new(Algorithm::EdDSA);
    validation.validate_exp = true;
    validation.set_required_spec_claims(&["exp"]);
    validation.set_audience(&["fleet"]);

    let token_data = decode::<FleetClaims>(token, &decoding_key, &validation)
        .map_err(|_| FleetAuthError::InvalidToken)?;

    let claims = token_data.claims;

    let op = claims.op.unwrap_or_default();
    if op.is_empty() || op != self_operator_id {
        return Err(FleetAuthError::OperatorMismatch);
    }

    let nid = claims.nid.unwrap_or_default();
    if nid.is_empty() {
        return Err(FleetAuthError::MissingNid);
    }

    Ok(FleetIdentity { nid, op })
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

    /// Serializable claim shape for signing test tokens. Mirrors the decoder
    /// shape but with required fields (None is encoded as absent via
    /// `skip_serializing_if`).
    #[derive(Debug, Serialize)]
    struct TestClaims {
        aud: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        op: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        nid: Option<String>,
        exp: u64,
    }

    struct Keypair {
        private_pem: String,
        public_pem: String,
    }

    fn generate_keypair() -> Keypair {
        // `SigningKey::generate` requires the `rand_core` feature (not enabled
        // on ed25519-dalek in this crate). Instead, pull raw bytes from the
        // `rand` crate's thread_rng and feed them into `SigningKey::from_bytes`.
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
        Keypair {
            private_pem,
            public_pem,
        }
    }

    fn future_exp() -> u64 {
        // Expire one hour from now.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time before unix epoch")
            .as_secs();
        now + 3600
    }

    fn past_exp() -> u64 {
        // Expired one hour ago.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time before unix epoch")
            .as_secs();
        now - 3600
    }

    fn sign_token(claims: &TestClaims, private_pem: &str) -> String {
        let header = Header::new(Algorithm::EdDSA);
        let encoding_key = EncodingKey::from_ed_pem(private_pem.as_bytes()).expect("encoding key");
        encode(&header, claims, &encoding_key).expect("sign token")
    }

    // ── Test 1: happy path ───────────────────────────────────────────────

    #[test]
    fn valid_token_with_matching_op_and_nonempty_nid_returns_identity() {
        let kp = generate_keypair();
        let claims = TestClaims {
            aud: "fleet".into(),
            op: Some("operator-alpha".into()),
            nid: Some("node-abc123".into()),
            exp: future_exp(),
        };
        let token = sign_token(&claims, &kp.private_pem);

        let identity =
            verify_fleet_identity(&token, &kp.public_pem, "operator-alpha").expect("should verify");

        assert_eq!(identity.nid(), "node-abc123");
        assert_eq!(identity.op(), "operator-alpha");
    }

    #[test]
    fn bearer_prefix_is_stripped() {
        let kp = generate_keypair();
        let claims = TestClaims {
            aud: "fleet".into(),
            op: Some("operator-alpha".into()),
            nid: Some("node-abc123".into()),
            exp: future_exp(),
        };
        let token = sign_token(&claims, &kp.private_pem);
        let bearer = format!("Bearer {}", token);

        let identity = verify_fleet_identity(&bearer, &kp.public_pem, "operator-alpha")
            .expect("should verify with Bearer prefix");
        assert_eq!(identity.nid(), "node-abc123");
    }

    // ── Test 2: wrong audience ───────────────────────────────────────────

    #[test]
    fn wrong_audience_returns_invalid_token() {
        let kp = generate_keypair();
        let claims = TestClaims {
            aud: "pyramid-query".into(), // NOT "fleet"
            op: Some("operator-alpha".into()),
            nid: Some("node-abc123".into()),
            exp: future_exp(),
        };
        let token = sign_token(&claims, &kp.private_pem);

        let err = verify_fleet_identity(&token, &kp.public_pem, "operator-alpha")
            .expect_err("wrong aud must reject");
        assert_eq!(err, FleetAuthError::InvalidToken);
    }

    // ── Test 3: expired token ────────────────────────────────────────────

    #[test]
    fn expired_token_returns_invalid_token() {
        let kp = generate_keypair();
        let claims = TestClaims {
            aud: "fleet".into(),
            op: Some("operator-alpha".into()),
            nid: Some("node-abc123".into()),
            exp: past_exp(),
        };
        let token = sign_token(&claims, &kp.private_pem);

        let err = verify_fleet_identity(&token, &kp.public_pem, "operator-alpha")
            .expect_err("expired token must reject");
        assert_eq!(err, FleetAuthError::InvalidToken);
    }

    // ── Test 4: op mismatch ──────────────────────────────────────────────

    #[test]
    fn op_mismatch_returns_operator_mismatch() {
        let kp = generate_keypair();
        let claims = TestClaims {
            aud: "fleet".into(),
            op: Some("operator-beta".into()),
            nid: Some("node-abc123".into()),
            exp: future_exp(),
        };
        let token = sign_token(&claims, &kp.private_pem);

        let err = verify_fleet_identity(&token, &kp.public_pem, "operator-alpha")
            .expect_err("op mismatch must reject");
        assert_eq!(err, FleetAuthError::OperatorMismatch);
    }

    #[test]
    fn missing_op_claim_returns_operator_mismatch() {
        let kp = generate_keypair();
        let claims = TestClaims {
            aud: "fleet".into(),
            op: None,
            nid: Some("node-abc123".into()),
            exp: future_exp(),
        };
        let token = sign_token(&claims, &kp.private_pem);

        let err = verify_fleet_identity(&token, &kp.public_pem, "operator-alpha")
            .expect_err("missing op must reject");
        assert_eq!(err, FleetAuthError::OperatorMismatch);
    }

    // ── Test 5: missing nid (None) ───────────────────────────────────────

    #[test]
    fn missing_nid_claim_returns_missing_nid() {
        let kp = generate_keypair();
        let claims = TestClaims {
            aud: "fleet".into(),
            op: Some("operator-alpha".into()),
            nid: None,
            exp: future_exp(),
        };
        let token = sign_token(&claims, &kp.private_pem);

        let err = verify_fleet_identity(&token, &kp.public_pem, "operator-alpha")
            .expect_err("missing nid must reject");
        assert_eq!(err, FleetAuthError::MissingNid);
    }

    // ── Test 6: empty-string nid ─────────────────────────────────────────

    #[test]
    fn empty_string_nid_returns_missing_nid() {
        let kp = generate_keypair();
        let claims = TestClaims {
            aud: "fleet".into(),
            op: Some("operator-alpha".into()),
            nid: Some(String::new()),
            exp: future_exp(),
        };
        let token = sign_token(&claims, &kp.private_pem);

        let err = verify_fleet_identity(&token, &kp.public_pem, "operator-alpha")
            .expect_err("empty nid must reject");
        assert_eq!(err, FleetAuthError::MissingNid);
    }

    // ── Test 7: self_operator_id empty ───────────────────────────────────

    #[test]
    fn empty_self_operator_id_returns_missing_operator() {
        let kp = generate_keypair();
        let claims = TestClaims {
            aud: "fleet".into(),
            op: Some("operator-alpha".into()),
            nid: Some("node-abc123".into()),
            exp: future_exp(),
        };
        let token = sign_token(&claims, &kp.private_pem);

        let err = verify_fleet_identity(&token, &kp.public_pem, "")
            .expect_err("empty self_operator_id must reject");
        assert_eq!(err, FleetAuthError::MissingOperator);
    }

    // ── Test 8: signature failure ────────────────────────────────────────

    #[test]
    fn signature_from_wrong_key_returns_invalid_token() {
        // Sign with one keypair, verify with another's public key.
        let signer_kp = generate_keypair();
        let verifier_kp = generate_keypair();

        let claims = TestClaims {
            aud: "fleet".into(),
            op: Some("operator-alpha".into()),
            nid: Some("node-abc123".into()),
            exp: future_exp(),
        };
        let token = sign_token(&claims, &signer_kp.private_pem);

        let err = verify_fleet_identity(&token, &verifier_kp.public_pem, "operator-alpha")
            .expect_err("wrong-key signature must reject");
        assert_eq!(err, FleetAuthError::InvalidToken);
    }

    // ── Extras: malformed inputs ─────────────────────────────────────────

    #[test]
    fn malformed_token_returns_invalid_token() {
        let kp = generate_keypair();
        let err = verify_fleet_identity("not.a.jwt", &kp.public_pem, "operator-alpha")
            .expect_err("malformed token must reject");
        assert_eq!(err, FleetAuthError::InvalidToken);
    }

    #[test]
    fn malformed_public_key_returns_invalid_token() {
        let err = verify_fleet_identity("some.token.here", "-----BEGIN NOT A KEY-----", "op")
            .expect_err("malformed pubkey must reject");
        assert_eq!(err, FleetAuthError::InvalidToken);
    }

    #[test]
    fn display_covers_all_variants() {
        // Smoke test that Display doesn't panic for any variant.
        for err in [
            FleetAuthError::InvalidToken,
            FleetAuthError::OperatorMismatch,
            FleetAuthError::MissingNid,
            FleetAuthError::MissingOperator,
        ] {
            let s = format!("{}", err);
            assert!(!s.is_empty(), "Display should produce non-empty string");
        }
    }
}
