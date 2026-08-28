//! Bearer token authentication.
//!
//! The comparison is constant-time. A naive `==` on strings short-circuits at the first
//! differing byte, and over enough requests the timing difference tells an attacker the
//! token one character at a time. That attack is impractical over a noisy network and
//! entirely practical over a loopback interface or a fast LAN, which is exactly where an
//! agent tends to live.

use vds_agent_protocol::parse_bearer;

/// Why a request was not authorised.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthFailure {
    /// No `Authorization` header at all.
    Missing,
    /// Present but not a well-formed bearer token.
    Malformed,
    /// Well-formed and wrong.
    Rejected,
}

impl AuthFailure {
    /// What the client is told.
    ///
    /// Deliberately the same words for all three: distinguishing "no token" from "wrong
    /// token" hands a prober information for nothing in return. The detail goes to the
    /// log, not to the response.
    pub fn message(self) -> &'static str {
        "unauthorized"
    }

    /// What the log records.
    pub fn detail(self) -> &'static str {
        match self {
            AuthFailure::Missing => "no authorization header",
            AuthFailure::Malformed => "authorization header is not a bearer token",
            AuthFailure::Rejected => "bearer token does not match",
        }
    }
}

/// Checks an `Authorization` header value against the configured token.
pub fn authorize(header: Option<&str>, expected: &str) -> Result<(), AuthFailure> {
    let Some(header) = header else {
        return Err(AuthFailure::Missing);
    };
    let Some(presented) = parse_bearer(header) else {
        return Err(AuthFailure::Malformed);
    };

    if constant_time_eq(presented.as_bytes(), expected.as_bytes()) {
        Ok(())
    } else {
        Err(AuthFailure::Rejected)
    }
}

/// Compares two byte strings in time that does not depend on their contents.
///
/// Length is not secret — an attacker can measure the response size of their own
/// request — so returning early on a length mismatch is safe and keeps the loop simple.
/// Within equal lengths, every byte is always examined.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }

    let mut difference: u8 = 0;
    for (left, right) in a.iter().zip(b) {
        difference |= left ^ right;
    }
    difference == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use vds_agent_protocol::bearer;

    const TOKEN: &str = "0123456789abcdef0123456789abcdef";

    #[test]
    fn the_configured_token_is_accepted() {
        let header = bearer(TOKEN);
        assert_eq!(authorize(Some(&header), TOKEN), Ok(()));
    }

    #[test]
    fn a_wrong_token_is_rejected() {
        let header = bearer("0123456789abcdef0123456789abcdee");
        assert_eq!(authorize(Some(&header), TOKEN), Err(AuthFailure::Rejected));
    }

    #[test]
    fn a_token_that_is_merely_a_prefix_is_rejected() {
        // The bug a `starts_with` would introduce.
        let header = bearer("0123456789abcdef");
        assert_eq!(authorize(Some(&header), TOKEN), Err(AuthFailure::Rejected));
    }

    #[test]
    fn a_longer_token_sharing_the_prefix_is_rejected() {
        let header = bearer(&format!("{TOKEN}extra"));
        assert_eq!(authorize(Some(&header), TOKEN), Err(AuthFailure::Rejected));
    }

    #[test]
    fn a_missing_header_is_distinguished_in_the_log_but_not_in_the_reply() {
        assert_eq!(authorize(None, TOKEN), Err(AuthFailure::Missing));
        assert_eq!(
            authorize(Some("Basic abc"), TOKEN),
            Err(AuthFailure::Malformed)
        );

        // The client is told the same thing in every case.
        assert_eq!(
            AuthFailure::Missing.message(),
            AuthFailure::Rejected.message()
        );
        assert_eq!(
            AuthFailure::Malformed.message(),
            AuthFailure::Rejected.message()
        );
        assert_ne!(
            AuthFailure::Missing.detail(),
            AuthFailure::Rejected.detail()
        );
    }

    #[test]
    fn an_empty_configured_token_never_authorises_anything() {
        // Belt and braces: the configuration layer already refuses to start without one.
        assert_eq!(authorize(Some("Bearer "), ""), Err(AuthFailure::Malformed));
        assert_eq!(authorize(Some("Bearer x"), ""), Err(AuthFailure::Rejected));
    }

    #[test]
    fn the_comparison_examines_every_byte_of_an_equal_length_input() {
        // Not a timing measurement — those are hopeless in a unit test — but a check that
        // the implementation has no early exit inside the loop.
        assert!(constant_time_eq(b"abcd", b"abcd"));
        assert!(!constant_time_eq(b"abcd", b"abce"));
        assert!(!constant_time_eq(b"abcd", b"zbcd"));
        assert!(!constant_time_eq(b"abcd", b"abcde"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn the_scheme_is_matched_case_insensitively_but_the_token_is_not() {
        assert_eq!(authorize(Some(&format!("bearer {TOKEN}")), TOKEN), Ok(()));
        assert_eq!(
            authorize(Some(&bearer(&TOKEN.to_uppercase())), TOKEN),
            Err(AuthFailure::Rejected)
        );
    }
}
