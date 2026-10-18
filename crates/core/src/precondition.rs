//! HTTP conditional request headers (`docs/mvp.md` §25).
//!
//! `If-Match` / `If-None-Match` are evaluated by the frontends (WebDAV, S3) but they must be
//! *applied* where the ETag cannot change any more: inside the document lock, right before the
//! write. Otherwise two concurrent conditional writes both observe the same ETag and both win.

use crate::error::{MeiSyncError, Result};

/// The raw `If-Match` / `If-None-Match` header values of one request.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Preconditions {
    pub if_match: Option<String>,
    pub if_none_match: Option<String>,
}

impl Preconditions {
    pub fn new(if_match: Option<&str>, if_none_match: Option<&str>) -> Self {
        Self {
            if_match: if_match.map(|v| v.to_string()),
            if_none_match: if_none_match.map(|v| v.to_string()),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.if_match.is_none() && self.if_none_match.is_none()
    }

    /// Whether `If-Match` allows the operation: no header, `*` on an existing resource, or an
    /// ETag that matches one of the listed candidates.
    pub fn if_match_holds(&self, current_etag: Option<&str>) -> bool {
        let raw = match &self.if_match {
            Some(raw) => raw.trim(),
            None => return true,
        };
        if raw == "*" {
            return current_etag.is_some();
        }
        let current = match current_etag {
            Some(etag) => normalize_etag(etag),
            None => return false,
        };
        raw.split(',').any(|candidate| normalize_etag(candidate) == current)
    }

    /// Whether `If-None-Match` matched an existing resource (→ `304` for reads, `412` for
    /// writes).
    pub fn none_match_matches(&self, current_etag: Option<&str>) -> bool {
        let raw = match &self.if_none_match {
            Some(raw) => raw.trim(),
            None => return false,
        };
        let current = match current_etag {
            Some(etag) => normalize_etag(etag),
            None => return false,
        };
        if raw == "*" {
            return true;
        }
        raw.split(',').any(|candidate| normalize_etag(candidate) == current)
    }

    /// Evaluate both headers as a write precondition; a failure is a `412`.
    pub fn check_write(&self, current_etag: Option<&str>) -> Result<()> {
        if !self.if_match_holds(current_etag) {
            return Err(MeiSyncError::PreconditionFailed(
                "If-Match does not match the current ETag".into(),
            ));
        }
        if self.none_match_matches(current_etag) {
            return Err(MeiSyncError::PreconditionFailed(
                "If-None-Match matches the current ETag".into(),
            ));
        }
        Ok(())
    }
}

/// Remove an optional `W/` prefix and the surrounding quotes of an ETag.
pub fn normalize_etag(value: &str) -> String {
    let value = value.trim();
    let value = value.strip_prefix("W/").unwrap_or(value);
    value.trim_matches('"').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_headers_always_hold() {
        let pre = Preconditions::default();
        assert!(pre.if_match_holds(None));
        assert!(!pre.none_match_matches(Some("\"abc\"")));
        assert!(pre.check_write(None).is_ok());
    }

    #[test]
    fn if_match_needs_the_current_etag() {
        let pre = Preconditions::new(Some("\"abc\""), None);
        assert!(pre.check_write(Some("\"abc\"")).is_ok());
        assert!(pre.check_write(Some("W/\"abc\"")).is_ok());
        assert!(pre.check_write(Some("\"other\"")).is_err());
        assert!(pre.check_write(None).is_err());
    }

    #[test]
    fn star_means_any_existing_resource() {
        let exists = Preconditions::new(Some("*"), None);
        assert!(exists.check_write(Some("\"abc\"")).is_ok());
        assert!(exists.check_write(None).is_err());

        let create_only = Preconditions::new(None, Some("*"));
        assert!(create_only.check_write(None).is_ok());
        assert!(create_only.check_write(Some("\"abc\"")).is_err());
    }

    #[test]
    fn if_none_match_compares_weakly() {
        let pre = Preconditions::new(None, Some("\"abc\""));
        assert!(pre.none_match_matches(Some("W/\"abc\"")));
        assert!(!pre.none_match_matches(Some("\"other\"")));
        assert!(!pre.none_match_matches(None));
    }
}
