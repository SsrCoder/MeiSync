//! Minimal XML helpers for the S3 API.

use chrono::{DateTime, Utc};

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

/// S3 timestamps are ISO-8601 with milliseconds and a `Z` suffix.
pub fn iso8601(value: DateTime<Utc>) -> String {
    value.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

/// HTTP-date used by the `Last-Modified` header.
pub fn http_date(value: DateTime<Utc>) -> String {
    value.format("%a, %d %b %Y %H:%M:%S GMT").to_string()
}

pub struct ObjectEntry<'a> {
    pub key: &'a str,
    pub size: u64,
    pub etag: String,
    pub last_modified: DateTime<Utc>,
    pub storage_class: &'a str,
}

pub fn write_object_entry(body: &mut String, entry: &ObjectEntry<'_>, content_type: &str) {
    body.push_str("<Contents>");
    body.push_str(&format!("<Key>{}</Key>", escape(entry.key)));
    body.push_str(&format!(
        "<LastModified>{}</LastModified>",
        iso8601(entry.last_modified)
    ));
    body.push_str(&format!("<ETag>{}</ETag>", escape(&entry.etag)));
    body.push_str(&format!("<Size>{}</Size>", entry.size));
    body.push_str(&format!(
        "<StorageClass>{}</StorageClass>",
        escape(entry.storage_class)
    ));
    body.push_str("<Owner><ID>meisync</ID></Owner>");
    body.push_str("<ContentType>");
    body.push_str(&escape(content_type));
    body.push_str("</ContentType>");
    body.push_str("</Contents>");
}