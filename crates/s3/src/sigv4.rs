//! AWS Signature Version 4 verification.
//!
//! MeiSync keeps S3 compatibility without an external identity provider: the S3 "secret access
//! key" is the MeiSync session token and the access key id is the account name (the token is
//! accepted as well). That way stock clients such as `aws s3`, `rclone` or `mc` keep working
//! while the account model stays "account + token" (`docs/mvp.md` §11).

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone)]
pub struct SigV4Request<'a> {
    pub method: &'a str,
    /// Raw (still percent-encoded) request path.
    pub path: &'a str,
    /// Raw query string as received.
    pub query: Option<&'a str>,
    pub headers: &'a BTreeMap<String, String>,
    pub body: &'a [u8],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SigV4Credentials {
    pub access_key: String,
    pub date: String,
    pub region: String,
    pub service: String,
    pub signed_headers: Vec<String>,
    pub signature: String,
}

#[derive(Debug)]
pub enum SigV4Error {
    Missing,
    Malformed(String),
    Mismatch,
}

impl std::fmt::Display for SigV4Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SigV4Error::Missing => write!(f, "missing authorization header"),
            SigV4Error::Malformed(reason) => write!(f, "malformed authorization header: {reason}"),
            SigV4Error::Mismatch => write!(f, "signature mismatch"),
        }
    }
}

/// Parse `Authorization: AWS4-HMAC-SHA256 ...`.
pub fn parse_authorization(header: &str) -> Result<SigV4Credentials, SigV4Error> {
    let header = header.trim();
    let rest = header
        .strip_prefix("AWS4-HMAC-SHA256")
        .ok_or_else(|| SigV4Error::Malformed("not an AWS4-HMAC-SHA256 header".into()))?;
    let mut credential = String::new();
    let mut signed_headers = String::new();
    let mut signature = String::new();
    for part in rest.split(',') {
        let part = part.trim();
        let (key, value) = match part.split_once('=') {
            Some((key, value)) => (key.trim(), value.trim()),
            None => continue,
        };
        match key {
            "Credential" => credential = value.to_string(),
            "SignedHeaders" => signed_headers = value.to_string(),
            "Signature" => signature = value.to_string(),
            _ => {}
        }
    }
    if credential.is_empty() || signature.is_empty() {
        return Err(SigV4Error::Malformed(
            "Credential and Signature are required".into(),
        ));
    }
    let scope: Vec<&str> = credential.split('/').collect();
    if scope.len() < 5 {
        return Err(SigV4Error::Malformed("invalid credential scope".into()));
    }
    Ok(SigV4Credentials {
        access_key: scope[0].to_string(),
        date: scope[1].to_string(),
        region: scope[2].to_string(),
        service: scope[3].to_string(),
        signed_headers: signed_headers
            .split(';')
            .filter(|s| !s.is_empty())
            .map(|s| s.to_ascii_lowercase())
            .collect(),
        signature,
    })
}

/// Recompute the signature and compare it with the one sent by the client.
pub fn verify(request: &SigV4Request<'_>, secret: &str) -> Result<SigV4Credentials, SigV4Error> {
    let auth = request
        .headers
        .get("authorization")
        .ok_or(SigV4Error::Missing)?;
    let credentials = parse_authorization(auth)?;

    let payload_hash = request
        .headers
        .get("x-amz-content-sha256")
        .cloned()
        .unwrap_or_else(|| {
            if credentials.signed_headers.iter().any(|h| h == "x-amz-content-sha256") {
                hex::encode(Sha256::digest(request.body))
            } else {
                "UNSIGNED-PAYLOAD".to_string()
            }
        });

    let amz_date = request
        .headers
        .get("x-amz-date")
        .cloned()
        .or_else(|| request.headers.get("date").cloned())
        .ok_or_else(|| SigV4Error::Malformed("missing x-amz-date".into()))?;

    let canonical_request = canonical_request(request, &credentials.signed_headers, &payload_hash);
    let scope = format!(
        "{}/{}/{}/aws4_request",
        credentials.date, credentials.region, credentials.service
    );
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{}\n{}\n{}",
        amz_date,
        scope,
        hex::encode(Sha256::digest(canonical_request.as_bytes()))
    );
    let signing_key = derive_key(secret, &credentials.date, &credentials.region, &credentials.service);
    let expected = hex::encode(hmac_sha256(&signing_key, string_to_sign.as_bytes()));

    if expected == credentials.signature {
        Ok(credentials)
    } else {
        Err(SigV4Error::Mismatch)
    }
}

fn canonical_request(
    request: &SigV4Request<'_>,
    signed_headers: &[String],
    payload_hash: &str,
) -> String {
    let mut canonical_headers = String::new();
    for name in signed_headers {
        let value = request
            .headers
            .get(name.as_str())
            .cloned()
            .unwrap_or_default();
        canonical_headers.push_str(name);
        canonical_headers.push(':');
        canonical_headers.push_str(value.trim());
        canonical_headers.push('\n');
    }
    format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        request.method,
        canonical_uri(request.path),
        canonical_query(request.query),
        canonical_headers,
        signed_headers.join(";"),
        payload_hash
    )
}

/// S3 uses the already encoded path, only normalising nothing.
fn canonical_uri(path: &str) -> String {
    if path.is_empty() {
        "/".to_string()
    } else {
        path.to_string()
    }
}

fn canonical_query(query: Option<&str>) -> String {
    let mut pairs: Vec<(String, String)> = Vec::new();
    if let Some(query) = query {
        for part in query.split('&') {
            if part.is_empty() {
                continue;
            }
            match part.split_once('=') {
                Some((key, value)) => pairs.push((key.to_string(), value.to_string())),
                None => pairs.push((part.to_string(), String::new())),
            }
        }
    }
    pairs.sort();
    // Note: an empty value still keeps its `=` (`prefix=`), exactly like the AWS SDKs do.
    pairs
        .into_iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<String>>()
        .join("&")
}

fn derive_key(secret: &str, date: &str, region: &str, service: &str) -> Vec<u8> {
    let k_date = hmac_sha256(format!("AWS4{secret}").as_bytes(), date.as_bytes());
    let k_region = hmac_sha256(&k_date, region.as_bytes());
    let k_service = hmac_sha256(&k_region, service.as_bytes());
    hmac_sha256(&k_service, b"aws4_request")
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_query_is_sorted() {
        assert_eq!(
            canonical_query(Some("prefix=a&list-type=2&max-keys=10")),
            "list-type=2&max-keys=10&prefix=a"
        );
    }

    #[test]
    fn canonical_query_keeps_empty_values() {
        // The AWS SDKs sign `prefix=` (with the `=`) – dropping it breaks the signature of
        // every `ListObjectsV2` call, because botocore always sends an empty prefix.
        assert_eq!(
            canonical_query(Some("list-type=2&prefix=&delimiter=%2F&encoding-type=url")),
            "delimiter=%2F&encoding-type=url&list-type=2&prefix="
        );
        assert_eq!(canonical_query(Some("acl")), "acl=");
    }

    #[test]
    fn signature_roundtrip() {
        let mut headers = BTreeMap::new();
        headers.insert("host".to_string(), "localhost:8080".to_string());
        headers.insert("x-amz-date".to_string(), "20240101T000000Z".to_string());
        headers.insert("x-amz-content-sha256".to_string(), "UNSIGNED-PAYLOAD".to_string());

        let method = "GET";
        let path = "/s3/alice/";
        let query = "list-type=2";
        let payload_hash = "UNSIGNED-PAYLOAD";
        let signed = vec!["host".to_string(), "x-amz-content-sha256".to_string(), "x-amz-date".to_string()];

        let canonical = canonical_request(
            &SigV4Request {
                method,
                path,
                query: Some(query),
                headers: &headers,
                body: &[],
            },
            &signed,
            payload_hash,
        );
        let scope = "20240101/us-east-1/s3/aws4_request";
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n20240101T000000Z\n{scope}\n{}",
            hex::encode(Sha256::digest(canonical.as_bytes()))
        );
        let key = derive_key("secret", "20240101", "us-east-1", "s3");
        let signature = hex::encode(hmac_sha256(&key, string_to_sign.as_bytes()));

        headers.insert(
            "authorization".to_string(),
            format!(
                "AWS4-HMAC-SHA256 Credential=alice/{scope}, SignedHeaders={}, Signature={signature}",
                signed.join(";")
            ),
        );
        let result = verify(
            &SigV4Request {
                method,
                path,
                query: Some(query),
                headers: &headers,
                body: &[],
            },
            "secret",
        );
        assert!(result.is_ok());
    }
}