//! Bearer token authentication for the MCP endpoint.
//!
//! The token is read once at startup from `MCP_AUTH_TOKEN`. An unset or empty
//! value disables authentication (the caller logs a warning), which keeps
//! existing LAN deployments working after an upgrade. Comparison is
//! constant-time so the token cannot be recovered through response timing.

/// Bearer-token checker built from the process environment.
pub struct BearerAuth {
    token: Option<String>,
}

impl BearerAuth {
    /// Read `MCP_AUTH_TOKEN`; empty / whitespace-only means "disabled".
    pub fn from_env() -> Self {
        let token = std::env::var("MCP_AUTH_TOKEN")
            .ok()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty());

        if let Some(t) = &token {
            if t.chars().count() < 16 {
                eprintln!(
                    "usb-mcp: WARNING: MCP_AUTH_TOKEN is only {} characters — use at least 16 random characters (e.g. `openssl rand -hex 32`)",
                    t.chars().count()
                );
            }
            if t.chars().any(char::is_whitespace) {
                eprintln!(
                    "usb-mcp: WARNING: MCP_AUTH_TOKEN contains whitespace — bearer credentials cannot contain whitespace, so no request can ever match this token"
                );
            }
        }

        Self { token }
    }

    /// True when a token is configured.
    pub fn enabled(&self) -> bool {
        self.token.is_some()
    }

    /// True when the request may proceed: authentication is disabled, or the
    /// `Authorization` header carries the configured bearer token.
    pub fn authorize(&self, authorization: Option<&str>) -> bool {
        match &self.token {
            None => true,
            Some(expected) => match bearer_credential(authorization) {
                Some(presented) => ct_eq(presented.as_bytes(), expected.as_bytes()),
                None => false,
            },
        }
    }

    #[cfg(test)]
    fn new(token: Option<&str>) -> Self {
        Self { token: token.map(str::to_string) }
    }
}

/// Extract the credential from an `Authorization: Bearer <token>` header.
/// The scheme is case-insensitive (RFC 7235) and extra whitespace is tolerated.
fn bearer_credential(header: Option<&str>) -> Option<&str> {
    let header = header?.trim();
    let (scheme, rest) = header.split_once(char::is_whitespace)?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let credential = rest.trim();
    if credential.is_empty() || credential.chars().any(char::is_whitespace) {
        return None;
    }
    Some(credential)
}

/// Byte-wise constant-time comparison (the length itself is not secret).
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn auth(token: &str) -> BearerAuth {
        BearerAuth::new(Some(token))
    }

    #[test]
    fn accepts_exact_bearer_header() {
        assert!(auth("s3cret-token").authorize(Some("Bearer s3cret-token")));
    }

    #[test]
    fn scheme_is_case_insensitive() {
        assert!(auth("s3cret-token").authorize(Some("bearer s3cret-token")));
        assert!(auth("s3cret-token").authorize(Some("BEARER s3cret-token")));
    }

    #[test]
    fn tolerates_extra_whitespace() {
        assert!(auth("s3cret-token").authorize(Some("  Bearer   s3cret-token  ")));
        assert!(auth("s3cret-token").authorize(Some("Bearer\ts3cret-token")));
    }

    #[test]
    fn rejects_wrong_token() {
        assert!(!auth("s3cret-token").authorize(Some("Bearer s3cret-tokeN")));
        assert!(!auth("s3cret-token").authorize(Some("Bearer s3cret-token-extra")));
        assert!(!auth("s3cret-token").authorize(Some("Bearer ")));
    }

    #[test]
    fn rejects_missing_or_malformed_headers() {
        assert!(!auth("s3cret-token").authorize(None));
        assert!(!auth("s3cret-token").authorize(Some("")));
        assert!(!auth("s3cret-token").authorize(Some("s3cret-token")));
        assert!(!auth("s3cret-token").authorize(Some("Basic s3cret-token")));
        assert!(!auth("s3cret-token").authorize(Some("Bearer")));
        assert!(!auth("s3cret-token").authorize(Some("Bearer two words")));
    }

    #[test]
    fn disabled_auth_accepts_everything() {
        let a = BearerAuth::new(None);
        assert!(!a.enabled());
        assert!(a.authorize(None));
        assert!(a.authorize(Some("Bearer anything")));
    }

    #[test]
    fn enabled_auth_reports_enabled() {
        assert!(auth("s3cret-token").enabled());
    }

    #[test]
    fn ct_eq_is_exact() {
        assert!(ct_eq(b"", b""));
        assert!(ct_eq(b"abc", b"abc"));
        assert!(!ct_eq(b"abc", b"abd"));
        assert!(!ct_eq(b"abc", b"ab"));
        assert!(!ct_eq(b"abc", b"abcd"));
    }
}
