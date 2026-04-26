//! Market JWT identity verification — single source of truth for
//! compute market (and later storage/relay) dispatches.
//!
//! The Phase 2 `/v1/compute/job-dispatch` handler (and any future
//! market-audience handlers) all must perform the same set of checks
//! on the incoming `wire_job_token`:
//!
//! 1. Decode an Ed25519-signed JWT with `aud = "compute"` and
//!    `validate_exp = true`.
//! 2. Require `claims.pid` to match the node's own `node_id` (both
//!    non-empty). `pid` is the provider-binding claim — the Wire
//!    issues the token addressed to a specific provider, so a
//!    replay to a different provider is rejected.
//! 3. Require `claims.sub` to be present and non-empty. `sub` carries
//!    the `job_id` so the handler can correlate the dispatch body
//!    back to the JWT.
//!
//! Per `compute-market-architecture.md` §VIII.6 DD-F, the market and
//! fleet JWTs share the same Wire signing key; the `aud` claim is the
//! sole discriminator. This keeps the Wire's key management simple
//! (one signing key, rotate both audiences together).
//!
//! Parallel to `fleet_identity.rs`:
//!   - fleet:   aud = "fleet",   op-check (operator_id binding)
//!   - market:  aud = "compute", pid-check (provider_node_id binding)
//!   - (relay will reuse the same module with a different aud when
//!      the relay market ships.)

use std::fmt;

use jsonwebtoken::{decode, Algorithm, DecodingKey, Validation};
use serde::Deserialize;

/// Verified market identity extracted from a valid `wire_job_token`.
///
/// Guarantees:
/// - `sub_job_id` is non-empty (the job this dispatch is for).
/// - `pid` equals the verifying node's own `self_node_id` (non-empty).
///
/// Fields are private so `verify_market_identity` is the only
/// constructor — otherwise callers could build a
/// `MarketIdentity { pid: "".into(), sub_job_id: "".into() }` and
/// silently defeat the non-empty contract. Read via accessors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarketIdentity {
    /// Provider node_id (from JWT `pid` claim). Guaranteed equal to
    /// `self_node_id` (non-empty).
    pid: String,
    /// Job id the dispatch is for (from JWT `sub` claim). Guaranteed
    /// non-empty.
    sub_job_id: String,
}

impl MarketIdentity {
    /// Provider node_id (from JWT `pid` claim). Guaranteed equal to
    /// the `self_node_id` passed into the verifier.
    pub fn pid(&self) -> &str {
        &self.pid
    }

    /// Job id the dispatch is bound to (from JWT `sub` claim).
    /// Guaranteed non-empty.
    pub fn sub_job_id(&self) -> &str {
        &self.sub_job_id
    }
}

/// Failure modes for [`verify_market_identity`].
///
/// HTTP handlers convert every variant to a generic 401/403; the
/// specifics stay in logs / tracing metadata.
#[derive(Clone, PartialEq, Eq)]
pub enum MarketAuthError {
    /// Token failed decode: signature, audience, or expiration invalid.
    InvalidToken,
    /// `claims.pid` did not equal `self_node_id`. Replay-to-wrong-
    /// provider caught here.
    ProviderMismatch,
    /// `claims.sub` was absent or empty.
    MissingJobId,
    /// `self_node_id` passed into the verifier was empty.
    MissingSelfNodeId,
}

impl fmt::Debug for MarketAuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MarketAuthError::InvalidToken => write!(f, "MarketAuthError::InvalidToken"),
            MarketAuthError::ProviderMismatch => write!(f, "MarketAuthError::ProviderMismatch"),
            MarketAuthError::MissingJobId => write!(f, "MarketAuthError::MissingJobId"),
            MarketAuthError::MissingSelfNodeId => {
                write!(f, "MarketAuthError::MissingSelfNodeId")
            }
        }
    }
}

impl fmt::Display for MarketAuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MarketAuthError::InvalidToken => {
                write!(
                    f,
                    "market JWT decode failed (signature, aud, or exp invalid)"
                )
            }
            MarketAuthError::ProviderMismatch => {
                write!(f, "market JWT pid does not match this node's node_id")
            }
            MarketAuthError::MissingJobId => {
                write!(f, "market JWT missing or empty sub (job_id) claim")
            }
            MarketAuthError::MissingSelfNodeId => {
                write!(f, "local node_id is empty; cannot verify market identity")
            }
        }
    }
}

impl std::error::Error for MarketAuthError {}

/// Internal claims shape. Deliberately self-contained. Field names
/// are the canonical DD-F contract (`aud`, `iss`, `exp`, `iat`, `sub`,
/// `pid`); no `#[serde(alias = ...)]` intentional — aliases would
/// cross-contaminate with adjacent claim shapes (fleet `nid`,
/// document `sub` semantics).
#[derive(Debug, Deserialize)]
struct MarketClaims {
    // `aud` is enforced at decode time via `Validation::set_audience`.
    #[allow(dead_code)]
    aud: Option<String>,
    // `iss` is carried for observability but not enforced here —
    // callers that need issuer-binding do it at a higher layer.
    #[allow(dead_code)]
    iss: Option<String>,
    // `exp` is enforced at decode time via `Validation::validate_exp`.
    #[allow(dead_code)]
    exp: Option<u64>,
    // `iat` carried for observability only.
    #[allow(dead_code)]
    iat: Option<u64>,
    sub: Option<String>,
    pid: Option<String>,
}

/// Decode, verify, and normalize a `wire_job_token` into a typed
/// [`MarketIdentity`].
///
/// Checks performed:
/// 1. `self_node_id` non-empty (otherwise `MissingSelfNodeId`).
/// 2. `jsonwebtoken::decode` with `Algorithm::EdDSA`,
///    `set_audience(&["compute"])`, `validate_exp = true`. Any decode
///    failure → `InvalidToken`.
/// 3. `claims.pid == self_node_id` (otherwise `ProviderMismatch`).
/// 4. `claims.sub` is `Some(s)` with `!s.is_empty()` (otherwise
///    `MissingJobId`).
///
/// The `bearer_token` argument may include a leading `"Bearer "`
/// prefix, which is stripped before decode.
///
/// On success returns a [`MarketIdentity`] where both fields are
/// non-empty and `pid` is guaranteed to equal `self_node_id`.
pub fn verify_market_identity(
    bearer_token: &str,
    public_key: &str,
    self_node_id: &str,
) -> Result<MarketIdentity, MarketAuthError> {
    if self_node_id.is_empty() {
        return Err(MarketAuthError::MissingSelfNodeId);
    }

    let token = bearer_token.strip_prefix("Bearer ").unwrap_or(bearer_token);

    let decoding_key = DecodingKey::from_ed_pem(public_key.as_bytes())
        .map_err(|_| MarketAuthError::InvalidToken)?;

    let mut validation = Validation::new(Algorithm::EdDSA);
    validation.validate_exp = true;
    validation.set_required_spec_claims(&["exp"]);
    validation.set_audience(&["compute"]);

    let token_data = decode::<MarketClaims>(token, &decoding_key, &validation)
        .map_err(|_| MarketAuthError::InvalidToken)?;

    let claims = token_data.claims;

    let pid = claims.pid.unwrap_or_default();
    if pid.is_empty() || pid != self_node_id {
        return Err(MarketAuthError::ProviderMismatch);
    }

    let sub_job_id = claims.sub.unwrap_or_default();
    if sub_job_id.is_empty() {
        return Err(MarketAuthError::MissingJobId);
    }

    Ok(MarketIdentity { pid, sub_job_id })
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

    /// Serializable claim shape for signing test tokens. Mirrors the
    /// decoder shape; optional fields encoded absent via
    /// `skip_serializing_if`.
    #[derive(Debug, Serialize)]
    struct TestClaims {
        aud: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        iss: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        sub: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pid: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        iat: Option<u64>,
        exp: u64,
    }

    struct Keypair {
        private_pem: String,
        public_pem: String,
    }

    fn generate_keypair() -> Keypair {
        // Same pattern as fleet_identity tests — the `rand_core`
        // feature isn't enabled on ed25519-dalek here, so pull raw
        // bytes from `rand` thread_rng and feed them into
        // `SigningKey::from_bytes`.
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
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time before unix epoch")
            .as_secs()
            + 3600
    }

    fn past_exp() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time before unix epoch")
            .as_secs()
            - 3600
    }

    fn sign_token(claims: &TestClaims, private_pem: &str) -> String {
        let header = Header::new(Algorithm::EdDSA);
        let encoding_key = EncodingKey::from_ed_pem(private_pem.as_bytes()).expect("encoding key");
        encode(&header, claims, &encoding_key).expect("sign token")
    }

    fn default_claims() -> TestClaims {
        TestClaims {
            aud: "compute".into(),
            iss: Some("wire-signer".into()),
            sub: Some("job-abc123".into()),
            pid: Some("node-provider-alpha".into()),
            iat: Some(future_exp() - 3600),
            exp: future_exp(),
        }
    }

    // ── Happy path ──────────────────────────────────────────────────

    #[test]
    fn valid_token_with_matching_pid_and_nonempty_sub_returns_identity() {
        let kp = generate_keypair();
        let claims = default_claims();
        let token = sign_token(&claims, &kp.private_pem);

        let identity = verify_market_identity(&token, &kp.public_pem, "node-provider-alpha")
            .expect("should verify");

        assert_eq!(identity.pid(), "node-provider-alpha");
        assert_eq!(identity.sub_job_id(), "job-abc123");
    }

    #[test]
    fn bearer_prefix_is_stripped() {
        let kp = generate_keypair();
        let claims = default_claims();
        let token = sign_token(&claims, &kp.private_pem);
        let bearer = format!("Bearer {}", token);

        let identity = verify_market_identity(&bearer, &kp.public_pem, "node-provider-alpha")
            .expect("should verify with Bearer prefix");
        assert_eq!(identity.sub_job_id(), "job-abc123");
    }

    // ── Audience ─────────────────────────────────────────────────────

    #[test]
    fn wrong_audience_fleet_returns_invalid_token() {
        // A fleet-audience JWT must NOT be accepted by the market
        // verifier — DD-F's aud-as-discriminator contract. If this
        // fails the entire fleet/market separation is a lie.
        let kp = generate_keypair();
        let claims = TestClaims {
            aud: "fleet".into(),
            ..default_claims()
        };
        let token = sign_token(&claims, &kp.private_pem);
        let err = verify_market_identity(&token, &kp.public_pem, "node-provider-alpha")
            .expect_err("fleet aud must reject at market verifier");
        assert_eq!(err, MarketAuthError::InvalidToken);
    }

    #[test]
    fn wrong_audience_other_returns_invalid_token() {
        let kp = generate_keypair();
        let claims = TestClaims {
            aud: "pyramid-query".into(),
            ..default_claims()
        };
        let token = sign_token(&claims, &kp.private_pem);
        let err = verify_market_identity(&token, &kp.public_pem, "node-provider-alpha")
            .expect_err("non-compute aud must reject");
        assert_eq!(err, MarketAuthError::InvalidToken);
    }

    // ── Expiration ───────────────────────────────────────────────────

    #[test]
    fn expired_token_returns_invalid_token() {
        let kp = generate_keypair();
        let claims = TestClaims {
            exp: past_exp(),
            ..default_claims()
        };
        let token = sign_token(&claims, &kp.private_pem);
        let err = verify_market_identity(&token, &kp.public_pem, "node-provider-alpha")
            .expect_err("expired token must reject");
        assert_eq!(err, MarketAuthError::InvalidToken);
    }

    // ── pid binding (provider_node_id) ───────────────────────────────

    #[test]
    fn pid_mismatch_returns_provider_mismatch() {
        // Token was issued to a DIFFERENT provider — replay attack
        // caught here.
        let kp = generate_keypair();
        let claims = TestClaims {
            pid: Some("node-provider-beta".into()),
            ..default_claims()
        };
        let token = sign_token(&claims, &kp.private_pem);
        let err = verify_market_identity(&token, &kp.public_pem, "node-provider-alpha")
            .expect_err("pid mismatch must reject");
        assert_eq!(err, MarketAuthError::ProviderMismatch);
    }

    #[test]
    fn missing_pid_claim_returns_provider_mismatch() {
        let kp = generate_keypair();
        let claims = TestClaims {
            pid: None,
            ..default_claims()
        };
        let token = sign_token(&claims, &kp.private_pem);
        let err = verify_market_identity(&token, &kp.public_pem, "node-provider-alpha")
            .expect_err("missing pid must reject");
        assert_eq!(err, MarketAuthError::ProviderMismatch);
    }

    #[test]
    fn empty_string_pid_returns_provider_mismatch() {
        let kp = generate_keypair();
        let claims = TestClaims {
            pid: Some(String::new()),
            ..default_claims()
        };
        let token = sign_token(&claims, &kp.private_pem);
        let err = verify_market_identity(&token, &kp.public_pem, "node-provider-alpha")
            .expect_err("empty pid must reject");
        assert_eq!(err, MarketAuthError::ProviderMismatch);
    }

    // ── sub binding (job_id) ─────────────────────────────────────────

    #[test]
    fn missing_sub_claim_returns_missing_job_id() {
        let kp = generate_keypair();
        let claims = TestClaims {
            sub: None,
            ..default_claims()
        };
        let token = sign_token(&claims, &kp.private_pem);
        let err = verify_market_identity(&token, &kp.public_pem, "node-provider-alpha")
            .expect_err("missing sub must reject");
        assert_eq!(err, MarketAuthError::MissingJobId);
    }

    #[test]
    fn empty_string_sub_returns_missing_job_id() {
        let kp = generate_keypair();
        let claims = TestClaims {
            sub: Some(String::new()),
            ..default_claims()
        };
        let token = sign_token(&claims, &kp.private_pem);
        let err = verify_market_identity(&token, &kp.public_pem, "node-provider-alpha")
            .expect_err("empty sub must reject");
        assert_eq!(err, MarketAuthError::MissingJobId);
    }

    // ── Self node id ─────────────────────────────────────────────────

    #[test]
    fn empty_self_node_id_returns_missing_self_node_id() {
        let kp = generate_keypair();
        let claims = default_claims();
        let token = sign_token(&claims, &kp.private_pem);
        let err = verify_market_identity(&token, &kp.public_pem, "")
            .expect_err("empty self_node_id must reject");
        assert_eq!(err, MarketAuthError::MissingSelfNodeId);
    }

    // ── Signature / malformed inputs ─────────────────────────────────

    #[test]
    fn signature_from_wrong_key_returns_invalid_token() {
        let signer_kp = generate_keypair();
        let verifier_kp = generate_keypair();
        let claims = default_claims();
        let token = sign_token(&claims, &signer_kp.private_pem);
        let err = verify_market_identity(&token, &verifier_kp.public_pem, "node-provider-alpha")
            .expect_err("wrong-key signature must reject");
        assert_eq!(err, MarketAuthError::InvalidToken);
    }

    #[test]
    fn malformed_token_returns_invalid_token() {
        let kp = generate_keypair();
        let err = verify_market_identity("not.a.jwt", &kp.public_pem, "node-provider-alpha")
            .expect_err("malformed token must reject");
        assert_eq!(err, MarketAuthError::InvalidToken);
    }

    #[test]
    fn malformed_public_key_returns_invalid_token() {
        let err = verify_market_identity("some.token.here", "-----BEGIN NOT A KEY-----", "node-x")
            .expect_err("malformed pubkey must reject");
        assert_eq!(err, MarketAuthError::InvalidToken);
    }

    #[test]
    fn display_covers_all_variants() {
        for err in [
            MarketAuthError::InvalidToken,
            MarketAuthError::ProviderMismatch,
            MarketAuthError::MissingJobId,
            MarketAuthError::MissingSelfNodeId,
        ] {
            let s = format!("{}", err);
            assert!(!s.is_empty(), "Display should produce non-empty string");
        }
    }
}
