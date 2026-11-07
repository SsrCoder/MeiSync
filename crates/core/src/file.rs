use std::fmt;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use yrs::{Doc, GetString, Map, ReadTxn, Text, Transact, TransactionMut, WriteTxn};

use crate::user::UserId;

/// Root name of the shared text type inside every YDoc (`docs/mvp.md` §6).
pub const CONTENT_KEY: &str = "content";

/// Root name of the whole-value register inside a `Value` shaped YDoc.
///
/// A `Value` document keeps the complete file body in a single key of a `Y.Map`, so a write is one
/// atomic register replacement instead of a character splice (`docs/structured-json-docs.md` §4.2).
pub const VALUE_ROOT: &str = "value";
/// Key of the register inside [`VALUE_ROOT`].
pub const VALUE_KEY: &str = "value";

/// How a file's bytes are carried by its YDoc (`docs/structured-json-docs.md` §4.1).
///
/// ```text
/// Text   YDoc -> Y.Text("content")     human editable / line oriented (.md, .txt, .jsonl)
/// Value  YDoc -> Y.Map("value")        whole value replacement (application/json)
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContentShape {
    /// A single `Y.Text` root: characters merge, which is right for line oriented formats.
    Text,
    /// A single `Y.Map` root holding one key: whole-value writes are atomic registers.
    Value,
}

impl ContentShape {
    pub fn as_str(self) -> &'static str {
        match self {
            ContentShape::Text => "text",
            ContentShape::Value => "value",
        }
    }
}

/// The content shape a content type asks for.
///
/// Derived from the content type, never stored: `.json` (and any `application/json`) is a whole
/// value, everything else stays text. `application/x-ndjson` is deliberately *not* a value: those
/// files are line oriented and merge correctly as text.
pub fn content_shape(content_type: &str) -> ContentShape {
    let ct = content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if ct.starts_with("application/json") {
        ContentShape::Value
    } else {
        ContentShape::Text
    }
}

macro_rules! string_id {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }
            pub fn generate() -> Self {
                Self(uuid::Uuid::new_v4().to_string())
            }
            pub fn as_str(&self) -> &str {
                &self.0
            }
            pub fn into_string(self) -> String {
                self.0
            }
            /// Whether the id is a valid identifier, i.e. safe to be used as a
            /// directory name on the local filesystem.
            pub fn is_valid_fs_name(&self) -> bool {
                !self.0.is_empty()
                    && self.0.len() <= 128
                    && self
                        .0
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
                    && self.0 != "."
                    && self.0 != ".."
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_string())
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }
    };
}

string_id!(FileId);

/// File visibility (`docs/mvp.md` §12).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum Visibility {
    /// Only the owner may access the file through any protocol.
    #[default]
    Private,
    /// Readable by every authenticated user of this MeiSync instance.
    Public,
}

impl Visibility {
    pub fn as_i64(self) -> i64 {
        match self {
            Visibility::Private => 0,
            Visibility::Public => 1,
        }
    }
    pub fn from_i64(value: i64) -> Self {
        if value == 1 {
            Visibility::Public
        } else {
            Visibility::Private
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Visibility::Private => "private",
            Visibility::Public => "public",
        }
    }
}

/// Collections (directories) and files are both FileDocuments; a collection simply has no
/// meaningful content type / size and can not be synchronized as a blob.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum FileKind {
    #[default]
    File,
    Collection,
}

impl FileKind {
    pub fn as_i64(self) -> i64 {
        match self {
            FileKind::File => 0,
            FileKind::Collection => 1,
        }
    }
    pub fn from_i64(value: i64) -> Self {
        if value == 1 {
            FileKind::Collection
        } else {
            FileKind::File
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            FileKind::File => "file",
            FileKind::Collection => "collection",
        }
    }
}

/// Which YDoc synchronisation strategy a document uses (`docs/mvp.md` §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum SyncMode {
    /// Keep the full update log on disk and support efficient incremental sync.
    #[default]
    Incremental,
    /// Only the full state is kept: large / binary / chunked files.
    Full,
}

impl SyncMode {
    pub fn as_i64(self) -> i64 {
        match self {
            SyncMode::Incremental => 0,
            SyncMode::Full => 1,
        }
    }
    pub fn from_i64(value: i64) -> Self {
        if value == 1 {
            SyncMode::Full
        } else {
            SyncMode::Incremental
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            SyncMode::Incremental => "incremental",
            SyncMode::Full => "full",
        }
    }
}

/// How the bytes of a file are represented inside the single `Y.Text("content")` root.
///
/// The MVP content model is text only (`docs/mvp.md` §6), but the same tree must also carry the
/// binary files that §5.2 requires Full Sync for. Those are stored base64 encoded inside the
/// Y.Text, which keeps "one file = one YDoc" and makes every write a plain text transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentEncoding {
    /// UTF-8 text, stored verbatim in the Y.Text.
    Text,
    /// Arbitrary bytes, stored base64 encoded in the Y.Text.
    Base64,
}

impl ContentEncoding {
    pub fn as_str(self) -> &'static str {
        match self {
            ContentEncoding::Text => "text",
            ContentEncoding::Base64 => "base64",
        }
    }
}

/// Content types that are stored as plain text and therefore support CRDT incremental updates
/// (`docs/mvp.md` §5.1).
pub fn content_encoding(content_type: Option<&str>) -> ContentEncoding {
    let ct = match content_type {
        Some(ct) => ct
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase(),
        None => return ContentEncoding::Base64,
    };
    if ct.starts_with("text/") {
        return ContentEncoding::Text;
    }
    match ct.as_str() {
        "application/json"
        | "application/x-ndjson"
        | "application/yaml"
        | "application/x-yaml"
        | "application/toml"
        | "application/xml"
        | "application/javascript"
        | "application/ecmascript"
        | "application/typescript"
        | "application/x-sh"
        | "application/csv"
        | "application/x-sql"
        | "application/graphql" => ContentEncoding::Text,
        // Everything else (binary, images, archives, unknown types) is full state sync (§5.2).
        _ => ContentEncoding::Base64,
    }
}

/// Content types that carry no information about the payload.
///
/// HTTP clients fall back to one of these whenever they do not know (or do not care) what they
/// send: `curl --data-binary` sends `application/x-www-form-urlencoded`, Apifox / Postman file
/// uploads and most S3 SDKs send `application/octet-stream`. They must not be allowed to decide
/// the storage encoding on their own, because that silently turns a markdown file into a base64
/// blob (§5.2) — the file name is the better witness there.
pub const GENERIC_CONTENT_TYPES: &[&str] = &[
    "application/octet-stream",
    "binary/octet-stream",
    "application/binary",
    // curl's default for --data / --data-binary / --data-raw, not a file type.
    "application/x-www-form-urlencoded",
    // Download-forcing types some browsers and proxies attach to any file.
    "application/force-download",
    "application/download",
];

/// Effective content type of a write: the declared type wins, unless it is a generic
/// "unknown bytes" type, in which case the extension decides.
///
/// ```text
///   text/markdown + notes.md      -> text/markdown   (declared, parameters stripped)
///   octet-stream  + notes.md      -> text/markdown   (extension wins: Apifox, curl, S3 SDKs)
///   octet-stream  + logo.png      -> image/png       (extension wins)
///   octet-stream  + blob          -> octet-stream    (nothing to go by: stay binary)
///   image/png     + notes.md      -> image/png       (a specific declared type always wins)
/// ```
///
/// Only the storage *encoding* and `sync_mode` depend on this (§5.1, §5.2); it is also what ends
/// up in `files.content_type`, so `GET /dav` and S3 responses report the resolved type.
pub fn resolve_content_type(content_type: Option<&str>, name: &str) -> String {
    let declared = content_type
        .map(|ct| {
            ct.split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase()
        })
        .filter(|ct| !ct.is_empty());
    match declared {
        Some(ct) if !GENERIC_CONTENT_TYPES.contains(&ct.as_str()) => ct,
        // No type, or a generic one: the extension is the only real witness.
        _ => guess_content_type(name),
    }
}

/// Bytes -> Y.Text payload.
pub fn encode_content(encoding: ContentEncoding, bytes: &[u8]) -> String {
    match encoding {
        ContentEncoding::Text => String::from_utf8_lossy(bytes).to_string(),
        ContentEncoding::Base64 => {
            use base64::Engine;
            base64::engine::general_purpose::STANDARD.encode(bytes)
        }
    }
}

/// Y.Text payload -> bytes.
pub fn decode_content(encoding: ContentEncoding, text: &str) -> Vec<u8> {
    match encoding {
        ContentEncoding::Text => text.as_bytes().to_vec(),
        ContentEncoding::Base64 => {
            use base64::Engine;
            base64::engine::general_purpose::STANDARD
                .decode(text.as_bytes())
                // Tolerant fallback: documents written as plain text stay readable.
                .unwrap_or_else(|_| text.as_bytes().to_vec())
        }
    }
}

/// Decides whether a content type is suited for CRDT incremental updates (`docs/mvp.md` §5).
pub fn guess_sync_mode(content_type: Option<&str>) -> SyncMode {
    match content_encoding(content_type) {
        ContentEncoding::Text => SyncMode::Incremental,
        ContentEncoding::Base64 => SyncMode::Full,
    }
}

/// Server side metadata of a FileDocument.
///
/// Fields beyond the ones listed in `docs/mvp.md` §3.1 (`kind`, `size`, `etag`, `version`,
/// `sync_mode`) are caches derived from the YDoc; they exist so that directory listings
/// (WebDAV PROPFIND / S3 ListObjects) never have to materialise every document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileMetadata {
    pub id: FileId,
    pub parent_id: Option<FileId>,
    pub name: String,
    pub owner_id: UserId,
    pub visibility: Visibility,
    pub content_type: Option<String>,
    pub kind: FileKind,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Cached materialised content length in bytes (0 for collections).
    pub size: u64,
    /// Cached strong ETag: hash of the current YDoc state (`docs/mvp.md` §26).
    pub etag: Option<String>,
    /// Monotonically increasing per-document revision, bumped on every YDoc change.
    pub version: i64,
    pub sync_mode: SyncMode,
}

impl FileMetadata {
    pub fn is_collection(&self) -> bool {
        self.kind == FileKind::Collection
    }

    pub fn timestamp(&self) -> i64 {
        self.updated_at.timestamp_millis()
    }

    /// Sanitised file name: rejects path separators and traversal attempts.
    pub fn validate_name(name: &str) -> crate::error::Result<()> {
        if name.is_empty() {
            return Err(crate::error::MeiSyncError::bad_request("empty file name"));
        }
        if name == "." || name == ".." {
            return Err(crate::error::MeiSyncError::bad_request(format!(
                "illegal file name: {name}"
            )));
        }
        if name.contains('/') || name.contains('\\') || name.contains('\0') {
            return Err(crate::error::MeiSyncError::bad_request(format!(
                "illegal file name: {name}"
            )));
        }
        // Control characters (a percent decoded `%0A`, for instance) must never reach the
        // persistence layer or an HTTP header value.
        if name.chars().any(|c| c.is_ascii_control()) {
            return Err(crate::error::MeiSyncError::bad_request(
                "illegal file name: control characters are not allowed",
            ));
        }
        if name.len() > 255 {
            return Err(crate::error::MeiSyncError::bad_request(
                "file name too long (max 255 bytes)",
            ));
        }
        Ok(())
    }
}

/// The central aggregate: metadata + the YDoc which owns the file content
/// (`docs/mvp.md` §3.1, §2.1, §2.2).
#[derive(Clone)]
pub struct FileDocument {
    pub id: FileId,
    pub metadata: FileMetadata,
    pub doc: Arc<Doc>,
}

impl FileDocument {
    pub fn new(metadata: FileMetadata, doc: Doc) -> Self {
        Self {
            id: metadata.id.clone(),
            metadata,
            doc: Arc::new(doc),
        }
    }

    /// Materialise the full file bytes out of the YDoc (`docs/mvp.md` §2.2).
    pub fn materialize(&self) -> Vec<u8> {
        self.materialize_string().into_bytes()
    }

    pub fn materialize_string(&self) -> String {
        document_content(
            &self.doc,
            content_shape(&self.effective_content_type()),
        )
    }

    /// Content type used when a client did not provide one.
    pub fn effective_content_type(&self) -> String {
        self.metadata
            .content_type
            .clone()
            .unwrap_or_else(|| guess_content_type(&self.metadata.name))
    }
}

impl std::fmt::Debug for FileDocument {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileDocument")
            .field("id", &self.id)
            .field("name", &self.metadata.name)
            .field("kind", &self.metadata.kind)
            .field("visibility", &self.metadata.visibility)
            .field("version", &self.metadata.version)
            .finish()
    }
}

/// Replace the whole YDoc content inside a single transaction.
///
/// This is the core of `replace_content()`: a WebDAV `PUT` / S3 `PutObject` never writes to the
/// filesystem directly, it turns the full body into a YDoc transaction (`docs/mvp.md` §7, §8).
///
/// * `ContentShape::Text` deletes the characters and inserts the new ones – two concurrent writers
///   splice, which is what line oriented formats want;
/// * `ContentShape::Value` overwrites one register – two concurrent writers produce exactly one
///   winning value, chosen by the CRDT, never a splice of both (`docs/structured-json-docs.md` §4).
pub fn replace_content(
    doc: &Doc,
    origin: &TransactionOrigin,
    shape: ContentShape,
    content: &str,
) -> Vec<u8> {
    let mut txn = doc.transact_mut_with(origin.clone());
    match shape {
        ContentShape::Text => {
            let text = txn.get_or_insert_text(CONTENT_KEY);
            let len = text.len(&txn);
            if len > 0 {
                text.remove_range(&mut txn, 0, len);
            }
            if !content.is_empty() {
                text.push(&mut txn, content);
            }
        }
        ContentShape::Value => {
            let map = txn.get_or_insert_map(VALUE_ROOT);
            map.insert(&mut txn, VALUE_KEY, content.to_string());
        }
    }
    txn.encode_update_v1()
}

/// Marker used to attribute a transaction to its origin. Yjs/Yrs keeps the *client* clock
/// separately – this is only an application level tag (`docs/mvp.md` §19).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransactionOrigin(pub String);

impl TransactionOrigin {
    pub fn remote(user: &UserId, kind: &str) -> Self {
        Self(format!("{kind}:{}", user.0))
    }
    pub fn system(kind: &str) -> Self {
        Self(format!("system:{kind}"))
    }
}

impl From<&str> for TransactionOrigin {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

impl From<TransactionOrigin> for yrs::Origin {
    fn from(value: TransactionOrigin) -> Self {
        yrs::Origin::from(value.0)
    }
}

/// Read a Y.Text root type from a document.
pub fn read_text(doc: &Doc) -> String {
    let txn = doc.transact();
    match txn.get_text(CONTENT_KEY) {
        Some(text) => text.get_string(&txn),
        None => String::new(),
    }
}

/// Read the whole-value register, if this document has one.
pub fn read_value(doc: &Doc) -> Option<String> {
    let txn = doc.transact();
    let map = txn.get_map(VALUE_ROOT)?;
    let out = map.get(&txn, VALUE_KEY)?;
    Some(out.to_string(&txn))
}

/// Materialised content of a document, in the shape its content type asks for.
///
/// This is the **only** place that turns a YDoc into file content: the read path of every protocol
/// (WebDAV GET, S3 GetObject, YSync, JSON API), the size cache and the restart path all go through
/// it (`docs/structured-json-docs.md` §5, 验收要点).
///
/// The fallback keeps two cases readable instead of silently empty:
///
/// * a document written before the value shape existed still holds its bytes in `Y.Text` and is
///   read from there until it is migrated (§7);
/// * a document whose content type changed shape (a `.json` file overwritten as an image) keeps
///   answering with whatever root actually holds content.
pub fn document_content(doc: &Doc, shape: ContentShape) -> String {
    let (primary, secondary) = match shape {
        ContentShape::Text => (read_text(doc), read_value(doc).unwrap_or_default()),
        ContentShape::Value => (
            read_value(doc).unwrap_or_default(),
            read_text(doc),
        ),
    };
    if primary.is_empty() {
        secondary
    } else {
        primary
    }
}

/// Text root type of a document, creating it if necessary (read-write transaction).
pub fn ensure_text(txn: &mut TransactionMut<'_>) -> yrs::TextRef {
    txn.get_or_insert_text(CONTENT_KEY)
}

/// Guess a content type from a file extension.
pub fn guess_content_type(name: &str) -> String {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    let ct = match ext.as_str() {
        "txt" | "log" | "text" => "text/plain",
        "md" | "markdown" => "text/markdown",
        "json" => "application/json",
        // NDJSON (one JSON object per line) is plain text and gets incremental CRDT sync.
        "jsonl" | "ndjson" => "application/x-ndjson",
        "yaml" | "yml" => "application/yaml",
        "toml" => "application/toml",
        "xml" => "application/xml",
        "csv" => "text/csv",
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "js" | "mjs" => "application/javascript",
        "ts" => "application/typescript",
        "rs" => "text/x-rust",
        "py" => "text/x-python",
        "go" => "text/x-go",
        "sh" => "application/x-sh",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "gz" => "application/gzip",
        "tar" => "application/x-tar",
        "bin" => "application/octet-stream",
        _ => "application/octet-stream",
    };
    ct.to_string()
}

#[cfg(test)]
mod tests {
    use super::{
        content_encoding, content_shape, document_content, guess_sync_mode, read_text, read_value,
        replace_content, resolve_content_type, ContentEncoding, ContentShape, SyncMode,
        TransactionOrigin,
    };
    use yrs::updates::decoder::Decode;
    use yrs::{Doc, ReadTxn, StateVector, Transact, Update};

    fn apply(target: &Doc, update: &[u8]) {
        let mut txn = target.transact_mut();
        txn.apply_update(Update::decode_v1(update).expect("decode"))
            .expect("apply");
    }

    #[test]
    fn json_is_a_value_shape() {
        // Whole-value files: the register shape is the only one that can replace them atomically.
        assert_eq!(content_shape("application/json"), ContentShape::Value);
        assert_eq!(
            content_shape("application/json; charset=utf-8"),
            ContentShape::Value
        );
        assert_eq!(content_shape(" Application/JSON "), ContentShape::Value);
        // Line oriented JSON stays text: it merges by line, which is what those files want.
        assert_eq!(content_shape("application/x-ndjson"), ContentShape::Text);
        assert_eq!(content_shape("text/markdown"), ContentShape::Text);
        assert_eq!(content_shape(""), ContentShape::Text);
    }

    #[test]
    fn a_value_shape_round_trips_through_one_register() {
        let doc = Doc::new();
        let origin = TransactionOrigin::system("test");

        let update = replace_content(&doc, &origin, ContentShape::Value, "{\"a\":1}");
        assert!(!update.is_empty());
        assert_eq!(document_content(&doc, ContentShape::Value), "{\"a\":1}");
        // The text root stays empty: a value document carries no characters at all.
        assert_eq!(read_text(&doc), "");
        assert_eq!(read_value(&doc).as_deref(), Some("{\"a\":1}"));

        // A second write replaces the register, it does not append to it.
        replace_content(&doc, &origin, ContentShape::Value, "{\"a\":2}");
        assert_eq!(document_content(&doc, ContentShape::Value), "{\"a\":2}");
    }

    #[test]
    fn a_text_shape_still_writes_characters() {
        let doc = Doc::new();
        let origin = TransactionOrigin::system("test");
        replace_content(&doc, &origin, ContentShape::Text, "hello");
        assert_eq!(document_content(&doc, ContentShape::Text), "hello");
        assert_eq!(read_value(&doc), None);
    }

    #[test]
    fn concurrent_value_writes_keep_exactly_one_value() {
        // Two writers that both start from the same state and write without seeing each other.
        let a = Doc::new();
        let b = Doc::new();
        let origin = TransactionOrigin::system("test");
        replace_content(&a, &origin, ContentShape::Value, "{\"v\":0}");
        let shared = a.transact().encode_state_as_update_v1(&StateVector::default());
        apply(&b, &shared);

        let from_a = replace_content(&a, &origin, ContentShape::Value, "{\"v\":\"A\"}");
        let from_b = replace_content(&b, &origin, ContentShape::Value, "{\"v\":\"B\"}");
        apply(&a, &from_b);
        apply(&b, &from_a);

        let winner = document_content(&a, ContentShape::Value);
        // One of the two values wins – never a splice of both, and both peers agree.
        assert!(
            winner == "{\"v\":\"A\"}" || winner == "{\"v\":\"B\"}",
            "expected one of the two values, got {winner}"
        );
        assert_eq!(winner, document_content(&b, ContentShape::Value));
    }

    #[test]
    fn a_value_document_that_was_never_migrated_reads_its_text() {
        // Documents written before the value shape existed still hold their bytes in Y.Text;
        // a read must never silently turn them into an empty file (§7).
        let doc = Doc::new();
        let origin = TransactionOrigin::system("test");
        replace_content(&doc, &origin, ContentShape::Text, "{\"legacy\":true}");
        assert_eq!(
            document_content(&doc, ContentShape::Value),
            "{\"legacy\":true}"
        );
    }

    #[test]
    fn a_specific_declared_type_wins() {
        assert_eq!(
            resolve_content_type(Some("text/markdown"), "notes.md"),
            "text/markdown"
        );
        assert_eq!(
            resolve_content_type(Some("text/markdown; charset=utf-8"), "notes.md"),
            "text/markdown"
        );
        assert_eq!(
            resolve_content_type(Some("image/png"), "notes.md"),
            "image/png"
        );
        assert_eq!(
            resolve_content_type(Some("  Application/JSON  "), "blob.bin"),
            "application/json"
        );
    }

    #[test]
    fn a_generic_type_defers_to_the_extension() {
        // The Apifox / curl / S3 SDK case: `application/octet-stream` on a markdown file.
        assert_eq!(
            resolve_content_type(Some("application/octet-stream"), "图床.md"),
            "text/markdown"
        );
        assert_eq!(
            resolve_content_type(Some("application/octet-stream"), "logo.png"),
            "image/png"
        );
        assert_eq!(
            resolve_content_type(Some("binary/octet-stream"), "a.json"),
            "application/json"
        );
        // JSONL uploads (common client default: octet-stream) must stay text, not base64.
        assert_eq!(
            resolve_content_type(Some("application/octet-stream"), "a.jsonl"),
            "application/x-ndjson"
        );
        assert_eq!(
            resolve_content_type(None, "export.ndjson"),
            "application/x-ndjson"
        );
        // curl's default for --data-binary is form encoding, which says nothing about the file.
        assert_eq!(
            resolve_content_type(Some("application/x-www-form-urlencoded"), "notes.md"),
            "text/markdown"
        );
        // Nothing to go by: stay binary rather than corrupt the bytes.
        assert_eq!(
            resolve_content_type(Some("application/octet-stream"), "blob"),
            "application/octet-stream"
        );
        assert_eq!(
            resolve_content_type(Some("application/octet-stream"), "blob.bin"),
            "application/octet-stream"
        );
    }

    #[test]
    fn a_missing_type_uses_the_extension() {
        assert_eq!(resolve_content_type(None, "notes.md"), "text/markdown");
        assert_eq!(resolve_content_type(None, "logo.png"), "image/png");
        assert_eq!(resolve_content_type(Some(""), "notes.md"), "text/markdown");
        assert_eq!(
            resolve_content_type(Some("  "), "notes.md"),
            "text/markdown"
        );
    }

    #[test]
    fn resolution_drives_the_storage_encoding() {
        let octet_stream_markdown =
            resolve_content_type(Some("application/octet-stream"), "notes.md");
        assert_eq!(
            content_encoding(Some(&octet_stream_markdown)),
            ContentEncoding::Text
        );
        assert_eq!(
            guess_sync_mode(Some(&octet_stream_markdown)),
            SyncMode::Incremental
        );

        let octet_stream_png = resolve_content_type(Some("application/octet-stream"), "logo.png");
        assert_eq!(
            content_encoding(Some(&octet_stream_png)),
            ContentEncoding::Base64
        );
        assert_eq!(guess_sync_mode(Some(&octet_stream_png)), SyncMode::Full);

        let octet_stream_jsonl = resolve_content_type(Some("application/octet-stream"), "a.jsonl");
        assert_eq!(
            content_encoding(Some(&octet_stream_jsonl)),
            ContentEncoding::Text
        );
        assert_eq!(
            guess_sync_mode(Some(&octet_stream_jsonl)),
            SyncMode::Incremental
        );
    }
}
