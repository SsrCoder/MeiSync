use std::time::{SystemTime, UNIX_EPOCH};

/// Format an RFC 7231 (IMF-fixdate) HTTP date, e.g. `Wed, 21 Oct 2015 07:28:00 GMT`.
pub fn format_http_date(time: SystemTime) -> String {
    let secs = time
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    format_epoch(secs)
}

pub fn format_epoch(secs: i64) -> String {
    use chrono::{DateTime, Utc};
    let dt: DateTime<Utc> = DateTime::from_timestamp(secs, 0).unwrap_or_else(Utc::now);
    dt.format("%a, %d %b %Y %H:%M:%S GMT").to_string()
}

pub fn format_millis(millis: i64) -> String {
    format_epoch(millis.div_euclid(1000))
}

/// Minimal XML text escaping.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Remove surrounding quotes from an ETag and compare weakly.
pub fn normalize_etag(value: &str) -> String {
    let value = value.trim();
    let value = value.strip_prefix("W/").unwrap_or(value);
    value.trim_matches('"').to_string()
}

/// RFC 7232 `If-Match` check: returns true when the request may proceed.
pub fn if_match_allows(headers: &axum::http::HeaderMap, current: Option<&str>) -> bool {
    let raw = match headers.get(axum::http::header::IF_MATCH) {
        Some(value) => value.to_str().unwrap_or("").to_string(),
        None => return true,
    };
    if raw.trim() == "*" {
        return current.is_some();
    }
    let current = match current {
        Some(value) => normalize_etag(value),
        None => return false,
    };
    raw.split(',')
        .map(normalize_etag)
        .any(|candidate| candidate == current)
}

/// RFC 7232 `If-None-Match`: returns true when the resource *matches* (=> 304 / 412).
pub fn if_none_match_matches(headers: &axum::http::HeaderMap, current: Option<&str>) -> bool {
    let raw = match headers.get(axum::http::header::IF_NONE_MATCH) {
        Some(value) => value.to_str().unwrap_or("").to_string(),
        None => return false,
    };
    if raw.trim() == "*" {
        return current.is_some();
    }
    let current = match current {
        Some(value) => normalize_etag(value),
        None => return false,
    };
    raw.split(',')
        .map(normalize_etag)
        .any(|candidate| candidate == current)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderMap;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(
                axum::http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                axum::http::HeaderValue::from_str(value).unwrap(),
            );
        }
        map
    }

    #[test]
    fn if_match_semantics() {
        assert!(if_match_allows(&HeaderMap::new(), Some("\"abc\"")));
        assert!(if_match_allows(
            &headers(&[("if-match", "\"abc\"")]),
            Some("\"abc\"")
        ));
        assert!(!if_match_allows(
            &headers(&[("if-match", "\"other\"")]),
            Some("\"abc\"")
        ));
        assert!(!if_match_allows(&headers(&[("if-match", "*")]), None));
    }

    #[test]
    fn if_none_match_semantics() {
        assert!(!if_none_match_matches(&HeaderMap::new(), Some("\"abc\"")));
        assert!(if_none_match_matches(
            &headers(&[("if-none-match", "\"abc\"")]),
            Some("\"abc\"")
        ));
        assert!(if_none_match_matches(
            &headers(&[("if-none-match", "*")]),
            Some("\"abc\"")
        ));
    }

    #[test]
    fn escapes_xml() {
        assert_eq!(escape("a<b>&\"c\""), "a&lt;b&gt;&amp;&quot;c&quot;");
    }
}