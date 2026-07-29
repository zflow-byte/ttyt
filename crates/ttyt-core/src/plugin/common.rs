//! Helpers shared across `VendorPlugin` implementations. Plain string
//! parsing rather than regex throughout -- these patterns are fixed and
//! this keeps them infallible (no `Regex::new` to handle/`unwrap`).

/// Extracts the token following the first `Version ` in `text` (e.g.
/// `"...Version 15.2(2)E7, ..."` -> `"15.2(2)E7"`). Used by every vendor
/// plugin whose banner follows this common `Version X` convention.
pub(crate) fn extract_version_token(text: &str) -> Option<String> {
    let idx = text.find("Version ")?;
    let after = &text[idx + "Version ".len()..];
    let end = after.find([',', ' ', '\r', '\n']).unwrap_or(after.len());
    let version = &after[..end];
    if version.is_empty() {
        None
    } else {
        Some(version.to_string())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn extracts_version_terminated_by_comma() {
        assert_eq!(
            extract_version_token("Cisco IOS Software, Version 15.2(2)E7, RELEASE"),
            Some("15.2(2)E7".to_string())
        );
    }

    #[test]
    fn extracts_version_terminated_by_end_of_string() {
        assert_eq!(
            extract_version_token("Some Software, Version 10.09"),
            Some("10.09".to_string())
        );
    }

    #[test]
    fn returns_none_when_no_version_keyword_present() {
        assert_eq!(extract_version_token("no version keyword here"), None);
    }
}
