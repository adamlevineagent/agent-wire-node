// pyramid/tunnel_url.rs — Validated, normalized tunnel URL newtype.
//
// Rationale (from async-fleet-dispatch.md "Core Primitives → TunnelUrl"):
//   Freeform strings don't leak past the roster ingress point. Construction is
//   the only normalization step; every downstream site that holds a tunnel URL
//   holds a `TunnelUrl`, not a `String`. The "did every ingress site normalize?"
//   audit finding disappears.
//
// Key rules enforced by `TunnelUrl::parse`:
//   * scheme MUST be `http` or `https` (no file://, ws://, ssh://, etc.)
//   * host MUST be present and non-empty
//   * empty strings are rejected
//   * trailing slash on the path is stripped (so tunnels that advertise
//     `https://example.com` and `https://example.com/` compare equal and
//     construct identical endpoint URLs)
//
// There is deliberately no `Default` impl — a default tunnel URL is meaningless,
// and the absence forces call sites to own the "what do I do when tunnel is
// missing?" question explicitly.
//
// Serde round-trips as a plain string through `parse`, so existing saved state
// files, heartbeat responses, and fleet announcements continue to interoperate
// with no schema migration.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use url::Url;

/// A validated, normalized tunnel URL. See module docs for construction rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TunnelUrl(Url);

impl TunnelUrl {
    /// Parse and normalize. See module docs for the rule set.
    pub fn parse(s: &str) -> Result<Self, TunnelUrlError> {
        if s.is_empty() {
            return Err(TunnelUrlError::Empty);
        }

        let mut url = Url::parse(s).map_err(TunnelUrlError::Parse)?;

        // Scheme must be http or https. `url::Url::parse` happily accepts
        // file://, ws://, custom schemes, etc.; tunnels are always HTTP(S).
        let scheme = url.scheme();
        if scheme != "http" && scheme != "https" {
            return Err(TunnelUrlError::InvalidScheme(scheme.to_string()));
        }

        // Host must be present and non-empty. `host_str` returns None for
        // relative-ish URLs and `Some("")` in edge cases we want to reject.
        match url.host_str() {
            Some(h) if !h.is_empty() => {}
            _ => return Err(TunnelUrlError::MissingHost),
        }

        // Normalize trailing slash: `https://example.com/` becomes
        // `https://example.com` (path "" in url crate terms for a root URL
        // after set_path("")). We only strip a single trailing slash from a
        // multi-segment path to preserve intent — "/api/" → "/api", but we
        // leave bare-root "/" as-is because url::Url always re-inserts "/"
        // for the root path when serializing. Rather than fight the url
        // crate's normalization, we accept that
        // `parse("https://x.com")` and `parse("https://x.com/")` both
        // round-trip through `as_str` as `"https://x.com/"`. PartialEq on
        // the underlying Url handles the equality contract.
        //
        // For non-root trailing slashes (e.g. `/api/`), strip so downstream
        // path matching is stable.
        let path = url.path().to_string();
        if path.len() > 1 && path.ends_with('/') {
            let trimmed = path.trim_end_matches('/');
            // `set_path` is infallible for strings the url crate already
            // accepted as a path.
            url.set_path(trimmed);
        }

        Ok(TunnelUrl(url))
    }

    /// Authority view — `(scheme, host, port)`. Used by callback validation
    /// to compare against a roster entry without string-level games.
    pub fn authority(&self) -> (&str, Option<&str>, Option<u16>) {
        (self.0.scheme(), self.0.host_str(), self.0.port())
    }

    /// Raw path of the underlying URL. Exposed for validators that need to
    /// match path prefixes (e.g. `validate_callback_url` pinning
    /// `/v1/fleet/result`).
    pub fn path(&self) -> &str {
        self.0.path()
    }

    /// Full string view — logging, heartbeat body construction, and any
    /// interop with code that still takes `&str`.
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    /// Build an endpoint URL by REPLACING the base path with
    /// `absolute_path`. Tunnels are assumed root-served, so callers pass
    /// e.g. `"/v1/fleet/result"` and receive `"https://host/v1/fleet/result"`
    /// regardless of any stray path on the base.
    ///
    /// This uses raw concatenation rather than `url::Url::join` because
    /// `join` has two footguns for this use case:
    ///   1. An absolute path passed to `join` REPLACES the path — which is
    ///      what we want — but a relative path (no leading `/`) is resolved
    ///      against the base, which is NOT what we want and is a silent
    ///      correctness bug if a caller forgets the `/`.
    ///   2. `join` can produce `//` artifacts between the host and path
    ///      for empty-path bases in some edge cases.
    /// The `debug_assert!` catches missing leading slashes in tests/dev
    /// builds; concatenation preserves absolute-path semantics regardless.
    pub fn endpoint(&self, absolute_path: &str) -> String {
        debug_assert!(
            absolute_path.starts_with('/'),
            "TunnelUrl::endpoint requires an absolute path starting with '/'"
        );
        format!(
            "{}://{}{}{}",
            self.0.scheme(),
            // host_str is Some(_) by construction — parse() rejects missing host.
            self.0.host_str().unwrap(),
            self.0
                .port()
                .map(|p| format!(":{}", p))
                .unwrap_or_default(),
            absolute_path
        )
    }
}

impl fmt::Display for TunnelUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0.as_str())
    }
}

impl Serialize for TunnelUrl {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.0.as_str().serialize(s)
    }
}

impl<'de> Deserialize<'de> for TunnelUrl {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        TunnelUrl::parse(&s).map_err(serde::de::Error::custom)
    }
}

/// Errors surfaced by `TunnelUrl::parse`.
///
/// Deliberately NOT using `thiserror` to keep this primitive dependency-light;
/// the impls are short enough to write by hand and the error variants rarely
/// change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TunnelUrlError {
    /// Empty string input.
    Empty,
    /// `url::Url::parse` failed (malformed URL).
    Parse(url::ParseError),
    /// Scheme was present but not `http` or `https`.
    InvalidScheme(String),
    /// Host was absent or empty.
    MissingHost,
}

impl fmt::Display for TunnelUrlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TunnelUrlError::Empty => write!(f, "tunnel URL is empty"),
            TunnelUrlError::Parse(e) => write!(f, "tunnel URL failed to parse: {}", e),
            TunnelUrlError::InvalidScheme(s) => {
                write!(f, "tunnel URL scheme must be http or https, got `{}`", s)
            }
            TunnelUrlError::MissingHost => write!(f, "tunnel URL is missing a host"),
        }
    }
}

impl std::error::Error for TunnelUrlError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            TunnelUrlError::Parse(e) => Some(e),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_https_url() {
        let t = TunnelUrl::parse("https://example.com").expect("should parse");
        // url::Url serializes bare-root hosts with a trailing slash in the
        // authority; this is url-crate-normal and we accept it.
        assert_eq!(t.as_str(), "https://example.com/");
    }

    #[test]
    fn parses_https_url_with_trailing_slash() {
        // Both forms must parse; they should compare equal to each other
        // (same authority + same path after normalization).
        let a = TunnelUrl::parse("https://example.com").expect("should parse");
        let b = TunnelUrl::parse("https://example.com/").expect("should parse");
        assert_eq!(a, b);
        assert_eq!(a.as_str(), b.as_str());
    }

    #[test]
    fn parses_http_url() {
        // http, not just https, is a valid tunnel scheme.
        let t = TunnelUrl::parse("http://localhost:8080").expect("should parse");
        assert_eq!(t.as_str(), "http://localhost:8080/");
    }

    #[test]
    fn strips_trailing_slash_from_non_root_path() {
        let t = TunnelUrl::parse("https://example.com/api/").expect("should parse");
        assert_eq!(t.path(), "/api");
        assert_eq!(t.as_str(), "https://example.com/api");
    }

    #[test]
    fn rejects_missing_scheme() {
        let err = TunnelUrl::parse("example.com").expect_err("should reject");
        // url::Url::parse reports this as a "relative URL without a base".
        assert!(
            matches!(err, TunnelUrlError::Parse(_)),
            "expected Parse variant, got {:?}",
            err
        );
    }

    #[test]
    fn rejects_empty_string() {
        let err = TunnelUrl::parse("").expect_err("should reject empty");
        assert_eq!(err, TunnelUrlError::Empty);
    }

    #[test]
    fn rejects_missing_host() {
        // `https://` has a scheme but no host.
        let err = TunnelUrl::parse("https://").expect_err("should reject missing host");
        // Depending on url crate version this surfaces as Parse or MissingHost;
        // either is acceptable — the point is that it does NOT become a valid
        // TunnelUrl.
        match err {
            TunnelUrlError::Parse(_) | TunnelUrlError::MissingHost => {}
            other => panic!("expected Parse or MissingHost, got {:?}", other),
        }
    }

    #[test]
    fn rejects_non_http_scheme() {
        let err = TunnelUrl::parse("ftp://example.com").expect_err("should reject ftp");
        assert!(
            matches!(err, TunnelUrlError::InvalidScheme(ref s) if s == "ftp"),
            "expected InvalidScheme(\"ftp\"), got {:?}",
            err
        );
    }

    #[test]
    fn rejects_file_scheme() {
        let err = TunnelUrl::parse("file:///etc/passwd").expect_err("should reject file");
        assert!(matches!(err, TunnelUrlError::InvalidScheme(_)));
    }

    #[test]
    fn endpoint_on_root_url() {
        let t = TunnelUrl::parse("https://example.com").expect("should parse");
        assert_eq!(
            t.endpoint("/v1/fleet/result"),
            "https://example.com/v1/fleet/result"
        );
    }

    #[test]
    fn endpoint_replaces_prefix_path() {
        // Root-served invariant: even if the tunnel URL carries a path,
        // endpoint() REPLACES it (doesn't append).
        let t = TunnelUrl::parse("https://example.com/some/prefix").expect("should parse");
        assert_eq!(
            t.endpoint("/v1/fleet/result"),
            "https://example.com/v1/fleet/result"
        );
    }

    #[test]
    fn endpoint_preserves_port() {
        let t = TunnelUrl::parse("http://localhost:8080").expect("should parse");
        assert_eq!(
            t.endpoint("/v1/fleet/result"),
            "http://localhost:8080/v1/fleet/result"
        );
    }

    #[test]
    fn authority_returns_scheme_host_port() {
        let t = TunnelUrl::parse("https://example.com:8443/ignored")
            .expect("should parse");
        let (scheme, host, port) = t.authority();
        assert_eq!(scheme, "https");
        assert_eq!(host, Some("example.com"));
        assert_eq!(port, Some(8443));
    }

    #[test]
    fn authority_no_explicit_port() {
        // url crate treats 443 as the default for https and does NOT surface
        // it via `port()` (returns None). That's the behavior we want for
        // authority comparisons: don't let `example.com` and
        // `example.com:443` compare unequal just because one wrote the
        // default port.
        let t = TunnelUrl::parse("https://example.com").expect("should parse");
        let (scheme, host, port) = t.authority();
        assert_eq!(scheme, "https");
        assert_eq!(host, Some("example.com"));
        assert_eq!(port, None);
    }

    #[test]
    fn serialize_emits_plain_string() {
        let t = TunnelUrl::parse("https://example.com").expect("should parse");
        let json = serde_json::to_string(&t).expect("should serialize");
        assert_eq!(json, "\"https://example.com/\"");
    }

    #[test]
    fn deserialize_round_trip() {
        let t: TunnelUrl =
            serde_json::from_str("\"https://example.com\"").expect("should deserialize");
        assert_eq!(t.as_str(), "https://example.com/");

        // Full round-trip: serialize then deserialize.
        let json = serde_json::to_string(&t).expect("serialize");
        let back: TunnelUrl = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(t, back);
    }

    #[test]
    fn deserialize_rejects_missing_scheme() {
        let r: Result<TunnelUrl, _> = serde_json::from_str("\"example.com\"");
        assert!(r.is_err(), "expected deserialize failure for missing scheme");
    }

    #[test]
    fn deserialize_rejects_empty_string() {
        let r: Result<TunnelUrl, _> = serde_json::from_str("\"\"");
        assert!(r.is_err(), "expected deserialize failure for empty string");
    }

    #[test]
    fn deserialize_rejects_non_http_scheme() {
        let r: Result<TunnelUrl, _> = serde_json::from_str("\"ssh://example.com\"");
        assert!(r.is_err(), "expected deserialize failure for ssh scheme");
    }

    #[test]
    fn display_matches_as_str() {
        let t = TunnelUrl::parse("https://example.com/api").expect("should parse");
        assert_eq!(format!("{}", t), t.as_str());
    }

    #[test]
    fn error_is_error_trait() {
        // Confirm TunnelUrlError satisfies std::error::Error — downstream
        // callers might want to `Box<dyn Error>` their error sites.
        fn assert_error<E: std::error::Error>() {}
        assert_error::<TunnelUrlError>();
    }

    #[test]
    fn parse_error_has_source() {
        // The Parse variant should expose its underlying url::ParseError
        // via Error::source for `?`-propagated diagnostics.
        let err = TunnelUrl::parse("not a url at all").expect_err("should reject");
        if let TunnelUrlError::Parse(_) = err {
            assert!(std::error::Error::source(&err).is_some());
        } else {
            panic!("expected Parse variant, got {:?}", err);
        }
    }
}
