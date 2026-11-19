//! An S3-compatible API for backup and sync tools (rclone, restic, the AWS
//! command line…). Each storage location on this computer is a bucket; an
//! object's key is its path. Path-style addressing, region `us-east-1`.
//!
//! Requests are signed with AWS Signature Version 4: the access key is a
//! token for programs' id and the secret key its secret. Supported: listing
//! buckets and objects (V1 and V2), getting, heading, putting (plain,
//! signed-chunked and unsigned-chunked bodies), copying and deleting
//! objects (deleted objects go to the HearthDrive trash), and multipart uploads.

use crate::auth::{AuthUser, Need};
use crate::programs::named_roots;
use crate::state::SharedState;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use futures_util::StreamExt;
use nexus_common::time::now_ms;
use nexus_common::types::{FileEntry, StorageRoot};
use nexus_security::Action;
use nexus_storage::paths;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};
use ring::{digest, hmac};
use std::collections::BTreeMap;
use std::path::PathBuf;
use tokio::io::AsyncWriteExt;
use tower::ServiceExt;
use tower_http::services::ServeFile;

const REGION: &str = "us-east-1";
const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
/// Requests signed more than this long ago (or ahead) are refused.
const CLOCK_SKEW_MS: i64 = 15 * 60 * 1000;
const MAX_KEYS: usize = 1000;

/// Everything but the unreserved characters, as AWS encodes.
const AWS: &AsciiSet = &NON_ALPHANUMERIC.remove(b'-').remove(b'_').remove(b'.').remove(b'~');
const AWS_PATH: &AsciiSet = &AWS.remove(b'/');

struct S3Error {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl S3Error {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self { status, code, message: message.into() }
    }
    fn denied() -> Self {
        Self::new(StatusCode::FORBIDDEN, "AccessDenied", "Access denied")
    }
    fn no_key() -> Self {
        Self::new(StatusCode::NOT_FOUND, "NoSuchKey", "The specified key does not exist.")
    }
    fn internal(e: impl std::fmt::Display) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "InternalError", e.to_string())
    }
}

impl From<nexus_common::NexusError> for S3Error {
    fn from(e: nexus_common::NexusError) -> Self {
        use nexus_common::NexusError as E;
        match e {
            E::NotFound(_) => Self::no_key(),
            E::PermissionDenied(m) => Self::new(StatusCode::FORBIDDEN, "AccessDenied", m),
            E::Unauthorized => Self::new(StatusCode::FORBIDDEN, "InvalidAccessKeyId", "Unknown access key"),
            E::InvalidInput(m) => Self::new(StatusCode::BAD_REQUEST, "InvalidArgument", m),
            other => Self::internal(other),
        }
    }
}

impl From<std::io::Error> for S3Error {
    fn from(e: std::io::Error) -> Self {
        Self::internal(e)
    }
}

impl IntoResponse for S3Error {
    fn into_response(self) -> Response {
        let body = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Error><Code>{}</Code><Message>{}</Message></Error>",
            self.code,
            esc(&self.message)
        );
        (self.status, [(header::CONTENT_TYPE, "application/xml")], body).into_response()
    }
}

type S3Result<T> = Result<T, S3Error>;

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
}

fn xml(body: String) -> Response {
    (StatusCode::OK, [(header::CONTENT_TYPE, "application/xml")], format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>{body}"))
        .into_response()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn sha256_hex(data: &[u8]) -> String {
    hex(digest::digest(&digest::SHA256, data).as_ref())
}

fn hmac_sign(key: &[u8], data: &str) -> Vec<u8> {
    hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, key), data.as_bytes()).as_ref().to_vec()
}

fn decode(s: &str) -> String {
    percent_decode_str(&s.replace('+', "%20")).decode_utf8_lossy().into_owned()
}

fn query_pairs(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (decode(k), decode(v))
        })
        .collect()
}

/// What signs a request, and the key to check chunk signatures with.
struct Signed {
    user: AuthUser,
    signing_key: Vec<u8>,
    seed_signature: String,
    amz_date: String,
    scope: String,
    payload: String,
}

/// Checks a Signature Version 4 request (header or presigned URL).
async fn verify(s: &SharedState, method: &Method, path: &str, query: &str, headers: &HeaderMap) -> S3Result<Signed> {
    let pairs = query_pairs(query);
    let q = |k: &str| pairs.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    let presigned = q("X-Amz-Algorithm").is_some();
    let (credential, signed_headers, signature, amz_date, payload) = if presigned {
        if q("X-Amz-Algorithm").as_deref() != Some("AWS4-HMAC-SHA256") {
            return Err(S3Error::new(StatusCode::BAD_REQUEST, "AuthorizationQueryParametersError", "Unsupported algorithm"));
        }
        (
            q("X-Amz-Credential").unwrap_or_default(),
            q("X-Amz-SignedHeaders").unwrap_or_default(),
            q("X-Amz-Signature").unwrap_or_default(),
            q("X-Amz-Date").unwrap_or_default(),
            "UNSIGNED-PAYLOAD".to_string(),
        )
    } else {
        let auth = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| S3Error::new(StatusCode::FORBIDDEN, "AccessDenied", "Requests must be signed (AWS Signature Version 4)"))?;
        let rest = auth
            .strip_prefix("AWS4-HMAC-SHA256 ")
            .ok_or_else(|| S3Error::new(StatusCode::BAD_REQUEST, "InvalidRequest", "Use AWS Signature Version 4"))?;
        let mut parts = BTreeMap::new();
        for p in rest.split(',') {
            if let Some((k, v)) = p.trim().split_once('=') {
                parts.insert(k.to_string(), v.to_string());
            }
        }
        let date = headers.get("x-amz-date").and_then(|v| v.to_str().ok()).unwrap_or_default().to_string();
        let payload = headers.get("x-amz-content-sha256").and_then(|v| v.to_str().ok()).unwrap_or("UNSIGNED-PAYLOAD").to_string();
        (
            parts.remove("Credential").unwrap_or_default(),
            parts.remove("SignedHeaders").unwrap_or_default(),
            parts.remove("Signature").unwrap_or_default(),
            date,
            payload,
        )
    };

    // Credential: key/date/region/service/aws4_request
    let mut cred = credential.splitn(2, '/');
    let access_key = cred.next().unwrap_or_default();
    let scope = cred.next().unwrap_or_default().to_string();
    let scope_parts: Vec<&str> = scope.split('/').collect();
    if scope_parts.len() != 4 || scope_parts[2] != "s3" || scope_parts[3] != "aws4_request" {
        return Err(S3Error::new(StatusCode::BAD_REQUEST, "AuthorizationHeaderMalformed", "Bad credential scope"));
    }
    let when = chrono::NaiveDateTime::parse_from_str(&amz_date, "%Y%m%dT%H%M%SZ")
        .map_err(|_| S3Error::new(StatusCode::BAD_REQUEST, "AuthorizationHeaderMalformed", "Bad date"))?
        .and_utc()
        .timestamp_millis();
    let now = now_ms();
    if presigned {
        let expires: i64 = q("X-Amz-Expires").and_then(|e| e.parse().ok()).unwrap_or(0);
        if now > when + expires.clamp(1, 7 * 24 * 3600) * 1000 || when > now + CLOCK_SKEW_MS {
            return Err(S3Error::new(StatusCode::FORBIDDEN, "AccessDenied", "Request has expired"));
        }
    } else if (now - when).abs() > CLOCK_SKEW_MS {
        return Err(S3Error::new(StatusCode::FORBIDDEN, "RequestTimeTooSkewed", "The difference between the request time and this computer's time is too large."));
    }

    let (user, _token, secret) = crate::tokens::user_for(s, access_key, None).await.map_err(|_| {
        S3Error::new(StatusCode::FORBIDDEN, "InvalidAccessKeyId", "The access key does not exist here.")
    })?;

    // The canonical request.
    let mut canon_query: Vec<(String, String)> = pairs
        .iter()
        .filter(|(k, _)| k != "X-Amz-Signature")
        .map(|(k, v)| (utf8_percent_encode(k, AWS).to_string(), utf8_percent_encode(v, AWS).to_string()))
        .collect();
    canon_query.sort();
    let canon_query = canon_query.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("&");
    let canon_path = utf8_percent_encode(&decode_path(path), AWS_PATH).to_string();
    let mut canon_headers = String::new();
    for name in signed_headers.split(';') {
        let values: Vec<String> = headers
            .get_all(name)
            .iter()
            .map(|v| v.to_str().unwrap_or_default().split_whitespace().collect::<Vec<_>>().join(" "))
            .collect();
        canon_headers.push_str(&format!("{name}:{}\n", values.join(",")));
    }
    let canonical = format!("{}\n{canon_path}\n{canon_query}\n{canon_headers}\n{signed_headers}\n{payload}", method.as_str());
    let to_sign = format!("AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}", sha256_hex(canonical.as_bytes()));
    let mut key = hmac_sign(format!("AWS4{secret}").as_bytes(), scope_parts[0]);
    for part in &scope_parts[1..] {
        key = hmac_sign(&key, part);
    }
    let expected = hex(&hmac_sign(&key, &to_sign));
    if blake3::hash(expected.as_bytes()) != blake3::hash(signature.as_bytes()) {
        return Err(S3Error::new(
            StatusCode::FORBIDDEN,
            "SignatureDoesNotMatch",
            "The request signature we calculated does not match the signature you provided.",
        ));
    }
    Ok(Signed { user, signing_key: key, seed_signature: signature, amz_date, scope, payload })
}

/// The path with S3's own encoding undone (S3 does not encode it twice).
fn decode_path(path: &str) -> String {
    percent_decode_str(path).decode_utf8_lossy().into_owned()
}

/// `/{bucket}/{key}` from the request path.
fn bucket_and_key(path: &str) -> (String, String) {
    let p = decode_path(path);
    let p = p.trim_start_matches('/');
    match p.split_once('/') {
        Some((b, k)) => (b.to_string(), k.to_string()),
        None => (p.to_string(), String::new()),
    }
}

async fn bucket(s: &SharedState, user: &AuthUser, name: &str) -> S3Result<StorageRoot> {
    let root = named_roots(s, user)
        .await
        .into_iter()
        .find(|(n, _)| n == name)
        .map(|(_, r)| r)
        .ok_or_else(|| S3Error::new(StatusCode::NOT_FOUND, "NoSuchBucket", "The specified bucket does not exist"))?;
    s.storage
        .ensure_online(&root)
        .map_err(|_| S3Error::new(StatusCode::SERVICE_UNAVAILABLE, "ServiceUnavailable", "This storage is offline"))?;
    Ok(root)
}

/// A key as a path inside the storage location.
fn key_path(key: &str) -> S3Result<String> {
    let rel = paths::normalize_relative(key.trim_end_matches('/'))
        .map_err(|e| S3Error::new(StatusCode::BAD_REQUEST, "InvalidArgument", e.to_string()))?;
    if rel.is_empty() || paths::is_hidden_internal(&rel) {
        return Err(S3Error::new(StatusCode::BAD_REQUEST, "InvalidArgument", "This key cannot be used"));
    }
    Ok(rel)
}

fn etag(f: &FileEntry) -> String {
    let base = match &f.content_hash {
        Some(h) if h.len() >= 32 => h[..32].to_string(),
        _ => blake3::hash(format!("{}:{}:{}", f.relative_path, f.size, f.modified_at.unwrap_or(0)).as_bytes()).to_hex()[..32].to_string(),
    };
    // Not an MD5: the "-1" keeps tools from comparing it with one.
    format!("\"{base}-1\"")
}

fn iso(ms: Option<i64>) -> String {
    chrono::DateTime::from_timestamp_millis(ms.unwrap_or(0)).unwrap_or_default().format("%Y-%m-%dT%H:%M:%S.000Z").to_string()
}

fn http_date(ms: Option<i64>) -> String {
    chrono::DateTime::from_timestamp_millis(ms.unwrap_or(0)).unwrap_or_default().format("%a, %d %b %Y %H:%M:%S GMT").to_string()
}

pub async fn handle(State(s): State<SharedState>, req: Request) -> Response {
    match dispatch(&s, req).await {
        Ok(r) => r,
        Err(e) => e.into_response(),
    }
}

async fn dispatch(s: &SharedState, req: Request) -> S3Result<Response> {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let query = req.uri().query().unwrap_or_default().to_string();
    let signed = verify(s, &method, &path, &query, req.headers()).await?;
    let user = signed.user.clone();
    user.require(Action::Read).map_err(|_| S3Error::denied())?;
    let pairs = query_pairs(&query);
    let has = |k: &str| pairs.iter().any(|(n, _)| n == k);
    let q = |k: &str| pairs.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    let (bucket_name, key) = bucket_and_key(&path);

    if bucket_name.is_empty() {
        return match method {
            Method::GET => list_buckets(s, &user).await,
            _ => Err(S3Error::new(StatusCode::METHOD_NOT_ALLOWED, "MethodNotAllowed", "Not allowed here")),
        };
    }
    let root = bucket(s, &user, &bucket_name).await?;

    if key.is_empty() {
        return match method {
            Method::GET if has("location") => {
                Ok(xml(format!("<LocationConstraint xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">{REGION}</LocationConstraint>")))
            }
            Method::GET if has("uploads") => Ok(xml(format!(
                "<ListMultipartUploadsResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Bucket>{}</Bucket><IsTruncated>false</IsTruncated></ListMultipartUploadsResult>",
                esc(&bucket_name)
            ))),
            Method::GET if has("versioning") => {
                Ok(xml("<VersioningConfiguration xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"/>".into()))
            }
            Method::GET => list_objects(s, &user, &root, &bucket_name, &pairs).await,
            Method::HEAD => Ok(StatusCode::OK.into_response()),
            Method::POST if has("delete") => delete_many(s, &user, &root, req).await,
            Method::PUT => Err(S3Error::new(StatusCode::CONFLICT, "BucketAlreadyOwnedByYou", "Buckets are storage locations: add one in HearthDrive")),
            _ => Err(S3Error::new(StatusCode::METHOD_NOT_ALLOWED, "MethodNotAllowed", "Not allowed on a bucket")),
        };
    }

    match method {
        Method::GET | Method::HEAD => get_object(s, &user, &root, &key, req).await,
        Method::PUT if has("partNumber") && has("uploadId") => {
            let n: u32 = q("partNumber").and_then(|n| n.parse().ok()).filter(|n| (1..=10_000).contains(n)).ok_or_else(|| {
                S3Error::new(StatusCode::BAD_REQUEST, "InvalidArgument", "Part numbers go from 1 to 10000")
            })?;
            put_part(s, &user, &root, &q("uploadId").unwrap_or_default(), n, &signed, req).await
        }
        Method::PUT if req.headers().contains_key("x-amz-copy-source") => copy_object(s, &user, &root, &key, req).await,
        Method::PUT => put_object(s, &user, &root, &key, &signed, req).await,
        Method::POST if has("uploads") => start_upload(s, &user, &root, &bucket_name, &key).await,
        Method::POST if has("uploadId") => finish_upload(s, &user, &root, &bucket_name, &key, &q("uploadId").unwrap_or_default(), req).await,
        Method::DELETE if has("uploadId") => abort_upload(s, &root, &q("uploadId").unwrap_or_default()).await,
        Method::DELETE => delete_object(s, &user, &root, &key).await,
        _ => Err(S3Error::new(StatusCode::METHOD_NOT_ALLOWED, "MethodNotAllowed", "Not allowed on an object")),
    }
}

async fn list_buckets(s: &SharedState, user: &AuthUser) -> S3Result<Response> {
    let mut out = String::from("<ListAllMyBucketsResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Owner><ID>nexus</ID><DisplayName>");
    out.push_str(&esc(&user.username));
    out.push_str("</DisplayName></Owner><Buckets>");
    for (name, root) in named_roots(s, user).await {
        out.push_str(&format!("<Bucket><Name>{}</Name><CreationDate>{}</CreationDate></Bucket>", esc(&name), iso(Some(root.created_at))));
    }
    out.push_str("</Buckets></ListAllMyBucketsResult>");
    Ok(xml(out))
}

enum Listed {
    Object(FileEntry),
    Prefix(String),
}

impl Listed {
    fn key(&self) -> &str {
        match self {
            Listed::Object(f) => &f.relative_path,
            Listed::Prefix(p) => p,
        }
    }
}

async fn list_objects(s: &SharedState, user: &AuthUser, root: &StorageRoot, bucket: &str, pairs: &[(String, String)]) -> S3Result<Response> {
    let q = |k: &str| pairs.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    let v2 = q("list-type").as_deref() == Some("2");
    let prefix = q("prefix").unwrap_or_default();
    let delimiter = q("delimiter").filter(|d| !d.is_empty());
    let max = q("max-keys").and_then(|m| m.parse::<usize>().ok()).unwrap_or(MAX_KEYS).min(MAX_KEYS);
    let url = q("encoding-type").as_deref() == Some("url");
    let after = if v2 {
        q("continuation-token").or_else(|| q("start-after")).unwrap_or_default()
    } else {
        q("marker").unwrap_or_default()
    };
    let enc = |k: &str| if url { utf8_percent_encode(k, AWS_PATH).to_string() } else { esc(k) };

    let mut items: Vec<Listed> = Vec::new();
    let mut truncated = false;
    match delimiter.as_deref() {
        Some("/") => {
            // One folder: its files, and its subfolders as common prefixes.
            let (parent, partial) = match prefix.rfind('/') {
                Some(i) => (&prefix[..i], &prefix[i + 1..]),
                None => ("", prefix.as_str()),
            };
            let children = s.db.folder_items(&root.id, parent, 200_000).await?;
            for f in children {
                if !f.filename.starts_with(partial) || paths::is_hidden_internal(&f.relative_path) {
                    continue;
                }
                if !user.may(&root.id, &f.relative_path, Need::Read) {
                    continue;
                }
                let item = if f.is_dir { Listed::Prefix(format!("{}/", f.relative_path)) } else { Listed::Object(f) };
                if item.key().starts_with(&prefix) && item.key() > after.as_str() {
                    items.push(item);
                }
            }
            items.sort_by(|a, b| a.key().cmp(b.key()));
            if items.len() > max {
                items.truncate(max);
                truncated = true;
            }
        }
        _ => {
            let mut from = after.clone();
            // Rows the caller may not see are skipped; fetch until a page fills.
            loop {
                let batch = s.db.object_list(&root.id, &prefix, &from, (max + 1) as i64).await?;
                let fetched = batch.len();
                for f in batch {
                    from = f.relative_path.clone();
                    if f.relative_path.starts_with(&prefix) && user.may(&root.id, &f.relative_path, Need::Read) {
                        if items.len() == max {
                            truncated = true;
                            break;
                        }
                        items.push(Listed::Object(f));
                    }
                }
                if truncated || fetched <= max {
                    break;
                }
            }
        }
    }

    let mut out = format!("<ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Name>{}</Name><Prefix>{}</Prefix>", esc(bucket), enc(&prefix));
    if let Some(d) = &delimiter {
        out.push_str(&format!("<Delimiter>{}</Delimiter>", enc(d)));
    }
    if url {
        out.push_str("<EncodingType>url</EncodingType>");
    }
    out.push_str(&format!("<MaxKeys>{max}</MaxKeys><IsTruncated>{truncated}</IsTruncated>"));
    let last = items.last().map(|i| i.key().to_string());
    if v2 {
        out.push_str(&format!("<KeyCount>{}</KeyCount>", items.len()));
        if let Some(t) = q("continuation-token") {
            out.push_str(&format!("<ContinuationToken>{}</ContinuationToken>", esc(&t)));
        }
        if let Some(sa) = q("start-after") {
            out.push_str(&format!("<StartAfter>{}</StartAfter>", enc(&sa)));
        }
        if truncated && let Some(l) = &last {
            out.push_str(&format!("<NextContinuationToken>{}</NextContinuationToken>", esc(l)));
        }
    } else {
        out.push_str(&format!("<Marker>{}</Marker>", enc(&after)));
        if truncated && let Some(l) = &last {
            out.push_str(&format!("<NextMarker>{}</NextMarker>", enc(l)));
        }
    }
    for item in &items {
        match item {
            Listed::Object(f) => out.push_str(&format!(
                "<Contents><Key>{}</Key><LastModified>{}</LastModified><ETag>{}</ETag><Size>{}</Size><StorageClass>STANDARD</StorageClass></Contents>",
                enc(&f.relative_path),
                iso(f.modified_at),
                esc(&etag(f)),
                f.size
            )),
            Listed::Prefix(p) => out.push_str(&format!("<CommonPrefixes><Prefix>{}</Prefix></CommonPrefixes>", enc(p))),
        }
    }
    out.push_str("</ListBucketResult>");
    Ok(xml(out))
}

async fn get_object(s: &SharedState, user: &AuthUser, root: &StorageRoot, key: &str, req: Request) -> S3Result<Response> {
    let rel = key_path(key)?;
    if !user.may(&root.id, &rel, Need::Read) {
        return Err(S3Error::denied());
    }
    let file = s.db.get_file_by_path(&root.id, &rel).await?.filter(|f| !f.is_dir);
    let path = s.storage.resolve(root, &rel)?;
    let meta = match tokio::fs::metadata(&path).await {
        Ok(m) if m.is_file() => m,
        _ => return Err(S3Error::no_key()),
    };
    let modified = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_millis() as i64);
    let tag = match &file {
        Some(f) if f.size == meta.len() as i64 => etag(f),
        _ => format!("\"{}-1\"", &blake3::hash(format!("{rel}:{}:{}", meta.len(), modified.unwrap_or(0)).as_bytes()).to_hex()[..32]),
    };
    let response = match ServeFile::new(&path).oneshot(req).await {
        Ok(r) => r,
        Err(never) => match never {},
    };
    let mut response = response.map(Body::new);
    let h = response.headers_mut();
    h.insert(header::ETAG, HeaderValue::from_str(&tag).unwrap_or(HeaderValue::from_static("\"\"")));
    if let Ok(v) = HeaderValue::from_str(&http_date(modified)) {
        h.insert(header::LAST_MODIFIED, v);
    }
    h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    Ok(response)
}

/// Turns an `aws-chunked` body back into the data, checking each chunk's
/// signature when the request was signed that way.
struct Chunked {
    buf: Vec<u8>,
    signed: bool,
    prev: String,
    key: Vec<u8>,
    date: String,
    scope: String,
    done: bool,
}

impl Chunked {
    fn new(sig: &Signed, signed: bool) -> Self {
        Self {
            buf: Vec::new(),
            signed,
            prev: sig.seed_signature.clone(),
            key: sig.signing_key.clone(),
            date: sig.amz_date.clone(),
            scope: sig.scope.clone(),
            done: false,
        }
    }

    /// Feeds bytes; returns the data of every chunk now complete.
    fn feed(&mut self, data: &[u8]) -> S3Result<Vec<u8>> {
        self.buf.extend_from_slice(data);
        let mut out = Vec::new();
        loop {
            if self.done {
                return Ok(out);
            }
            let Some(eol) = self.buf.windows(2).position(|w| w == b"\r\n") else { return Ok(out) };
            let line = String::from_utf8_lossy(&self.buf[..eol]).to_string();
            let (size_hex, ext) = line.split_once(';').unwrap_or((line.as_str(), ""));
            let size = usize::from_str_radix(size_hex.trim(), 16)
                .map_err(|_| S3Error::new(StatusCode::BAD_REQUEST, "IncompleteBody", "Bad chunk"))?;
            if size > 64 * 1024 * 1024 {
                return Err(S3Error::new(StatusCode::BAD_REQUEST, "InvalidRequest", "Chunk too large"));
            }
            let start = eol + 2;
            if self.buf.len() < start + size + if size == 0 { 0 } else { 2 } {
                return Ok(out);
            }
            let chunk = self.buf[start..start + size].to_vec();
            if self.signed {
                let given = ext.strip_prefix("chunk-signature=").unwrap_or_default();
                let to_sign = format!(
                    "AWS4-HMAC-SHA256-PAYLOAD\n{}\n{}\n{}\n{EMPTY_SHA256}\n{}",
                    self.date,
                    self.scope,
                    self.prev,
                    sha256_hex(&chunk)
                );
                let expected = hex(&hmac_sign(&self.key, &to_sign));
                if blake3::hash(expected.as_bytes()) != blake3::hash(given.as_bytes()) {
                    return Err(S3Error::new(StatusCode::FORBIDDEN, "SignatureDoesNotMatch", "A chunk's signature does not match"));
                }
                self.prev = expected;
            }
            if size == 0 {
                // Trailers (checksums) may follow; they are not needed.
                self.done = true;
                self.buf.clear();
                return Ok(out);
            }
            out.extend_from_slice(&chunk);
            self.buf.drain(..start + size + 2);
        }
    }
}

/// Writes a request body to a new file, checking it against the signed
/// payload hash. Returns the file, its size and an ETag.
async fn receive(s: &SharedState, root: &StorageRoot, sig: &Signed, req: Request) -> S3Result<(PathBuf, u64, String)> {
    let staging = std::path::Path::new(&root.absolute_path).join(paths::INTERNAL_DIR).join("staging");
    tokio::fs::create_dir_all(&staging).await?;
    let tmp = staging.join(format!("s3-{}.part", nexus_common::new_id()));
    let mut file = tokio::fs::File::create(&tmp).await?;
    let mode = sig.payload.as_str();
    let mut chunked = match mode {
        "STREAMING-AWS4-HMAC-SHA256-PAYLOAD" => Some(Chunked::new(sig, true)),
        "STREAMING-UNSIGNED-PAYLOAD-TRAILER" => Some(Chunked::new(sig, false)),
        _ => None,
    };
    let mut sha = digest::Context::new(&digest::SHA256);
    let mut tag = blake3::Hasher::new();
    let mut size = 0u64;
    let mut body = req.into_body().into_data_stream();
    let result: S3Result<()> = async {
        while let Some(piece) = body.next().await {
            let piece = piece.map_err(|e| S3Error::new(StatusCode::BAD_REQUEST, "IncompleteBody", e.to_string()))?;
            let data = match &mut chunked {
                Some(c) => c.feed(&piece)?,
                None => piece.to_vec(),
            };
            sha.update(&data);
            tag.update(&data);
            size += data.len() as u64;
            file.write_all(&data).await?;
        }
        file.flush().await?;
        if chunked.as_ref().is_some_and(|c| !c.done) {
            return Err(S3Error::new(StatusCode::BAD_REQUEST, "IncompleteBody", "The body ended early"));
        }
        let declared = mode.len() == 64 && mode.bytes().all(|b| b.is_ascii_hexdigit());
        if declared && hex(sha.clone().finish().as_ref()) != mode.to_ascii_lowercase() {
            return Err(S3Error::new(StatusCode::BAD_REQUEST, "XAmzContentSHA256Mismatch", "The content does not match its hash"));
        }
        Ok(())
    }
    .await;
    drop(file);
    if let Err(e) = result {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(e);
    }
    let _ = s;
    Ok((tmp, size, format!("\"{}-1\"", &tag.finalize().to_hex()[..32])))
}

fn may_write(user: &AuthUser, root: &StorageRoot, rel: &str) -> S3Result<()> {
    if root.read_only || user.require(Action::Write).is_err() || !user.may(&root.id, rel, Need::Write) {
        return Err(S3Error::denied());
    }
    Ok(())
}

/// Puts a finished file at its key (replacing what was there).
async fn place(s: &SharedState, root: &StorageRoot, rel: &str, tmp: &std::path::Path) -> S3Result<()> {
    let target = s.storage.resolve(root, rel)?;
    if tokio::fs::metadata(&target).await.is_ok_and(|m| m.is_dir()) {
        let _ = tokio::fs::remove_file(tmp).await;
        return Err(S3Error::new(StatusCode::CONFLICT, "InvalidRequest", "A folder has this name"));
    }
    if let Some(parent) = target.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    if let Err(e) = tokio::fs::rename(tmp, &target).await {
        let _ = tokio::fs::remove_file(tmp).await;
        return Err(e.into());
    }
    s.indexer.reindex_path(root, rel).await?;
    Ok(())
}

async fn put_object(s: &SharedState, user: &AuthUser, root: &StorageRoot, key: &str, sig: &Signed, req: Request) -> S3Result<Response> {
    let rel = key_path(key)?;
    may_write(user, root, &rel)?;
    // A key ending in "/" with no content is a folder.
    if key.ends_with('/') {
        let dir = s.storage.resolve(root, &rel)?;
        tokio::fs::create_dir_all(&dir).await?;
        s.indexer.reindex_path(root, &rel).await?;
        return Ok((StatusCode::OK, [(header::ETAG, "\"d41d8cd98f00b204e9800998ecf8427e\"")]).into_response());
    }
    let (tmp, _size, tag) = receive(s, root, sig, req).await?;
    place(s, root, &rel, &tmp).await?;
    Ok((StatusCode::OK, [(header::ETAG, tag)]).into_response())
}

async fn copy_object(s: &SharedState, user: &AuthUser, root: &StorageRoot, key: &str, req: Request) -> S3Result<Response> {
    let rel = key_path(key)?;
    may_write(user, root, &rel)?;
    let source = req.headers().get("x-amz-copy-source").and_then(|v| v.to_str().ok()).unwrap_or_default();
    let source = source.split('?').next().unwrap_or_default();
    let (src_bucket, src_key) = bucket_and_key(&format!("/{}", source.trim_start_matches('/')));
    let src_root = bucket(s, user, &src_bucket).await?;
    let src_rel = key_path(&src_key)?;
    if !user.may(&src_root.id, &src_rel, Need::Read) {
        return Err(S3Error::denied());
    }
    let from = s.storage.resolve(&src_root, &src_rel)?;
    if !tokio::fs::metadata(&from).await.is_ok_and(|m| m.is_file()) {
        return Err(S3Error::no_key());
    }
    let staging = std::path::Path::new(&root.absolute_path).join(paths::INTERNAL_DIR).join("staging");
    tokio::fs::create_dir_all(&staging).await?;
    let tmp = staging.join(format!("s3-{}.part", nexus_common::new_id()));
    tokio::fs::copy(&from, &tmp).await?;
    place(s, root, &rel, &tmp).await?;
    let file = s.db.get_file_by_path(&root.id, &rel).await?.ok_or_else(S3Error::no_key)?;
    Ok(xml(format!(
        "<CopyObjectResult><LastModified>{}</LastModified><ETag>{}</ETag></CopyObjectResult>",
        iso(file.modified_at),
        esc(&etag(&file))
    )))
}

async fn delete_object(s: &SharedState, user: &AuthUser, root: &StorageRoot, key: &str) -> S3Result<Response> {
    let rel = key_path(key)?;
    if root.read_only || user.require(Action::Write).is_err() || !user.may(&root.id, &rel, Need::Change) {
        return Err(S3Error::denied());
    }
    if let Some(file) = s.db.get_file_by_path(&root.id, &rel).await? {
        // Folders only go when empty, as in S3 they hold nothing themselves.
        if !file.is_dir || s.db.folder_items(&root.id, &rel, 1).await?.is_empty() {
            s.storage.trash(&file, user.user_id.as_deref()).await?;
            s.events.emit(nexus_common::events::Event::FileDeleted { root_id: root.id.clone(), path: rel });
        }
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// `<Key>` values in a request document.
fn keys_in(body: &str, tag: &str) -> Vec<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    body.split(&open)
        .skip(1)
        .filter_map(|p| p.split(&close).next())
        .map(|k| k.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&"))
        .collect()
}

async fn read_small_body(req: Request) -> S3Result<String> {
    let bytes = axum::body::to_bytes(req.into_body(), 4 * 1024 * 1024)
        .await
        .map_err(|e| S3Error::new(StatusCode::BAD_REQUEST, "MalformedXML", e.to_string()))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

async fn delete_many(s: &SharedState, user: &AuthUser, root: &StorageRoot, req: Request) -> S3Result<Response> {
    let body = read_small_body(req).await?;
    let quiet = body.contains("<Quiet>true</Quiet>");
    let mut out = String::from("<DeleteResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">");
    for key in keys_in(&body, "Key") {
        match delete_object(s, user, root, &key).await {
            Ok(_) if quiet => {}
            Ok(_) => out.push_str(&format!("<Deleted><Key>{}</Key></Deleted>", esc(&key))),
            Err(e) => out.push_str(&format!("<Error><Key>{}</Key><Code>{}</Code><Message>{}</Message></Error>", esc(&key), e.code, esc(&e.message))),
        }
    }
    out.push_str("</DeleteResult>");
    Ok(xml(out))
}

fn upload_dir(root: &StorageRoot, id: &str) -> S3Result<PathBuf> {
    if id.len() != 32 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(S3Error::new(StatusCode::NOT_FOUND, "NoSuchUpload", "The specified upload does not exist."));
    }
    Ok(std::path::Path::new(&root.absolute_path).join(paths::INTERNAL_DIR).join("staging").join(format!("s3-upload-{id}")))
}

async fn start_upload(s: &SharedState, user: &AuthUser, root: &StorageRoot, bucket: &str, key: &str) -> S3Result<Response> {
    let rel = key_path(key)?;
    may_write(user, root, &rel)?;
    let id = blake3::hash(format!("{}{}{}", nexus_common::new_id(), rel, now_ms()).as_bytes()).to_hex()[..32].to_string();
    let dir = upload_dir(root, &id)?;
    tokio::fs::create_dir_all(&dir).await?;
    tokio::fs::write(dir.join("key"), rel.as_bytes()).await?;
    let _ = s;
    Ok(xml(format!(
        "<InitiateMultipartUploadResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Bucket>{}</Bucket><Key>{}</Key><UploadId>{id}</UploadId></InitiateMultipartUploadResult>",
        esc(bucket),
        esc(key)
    )))
}

async fn put_part(s: &SharedState, user: &AuthUser, root: &StorageRoot, id: &str, n: u32, sig: &Signed, req: Request) -> S3Result<Response> {
    let dir = upload_dir(root, id)?;
    let rel = tokio::fs::read_to_string(dir.join("key"))
        .await
        .map_err(|_| S3Error::new(StatusCode::NOT_FOUND, "NoSuchUpload", "The specified upload does not exist."))?;
    may_write(user, root, &rel)?;
    let (tmp, _, tag) = receive(s, root, sig, req).await?;
    tokio::fs::rename(&tmp, dir.join(format!("{n:05}.part"))).await?;
    tokio::fs::write(dir.join(format!("{n:05}.etag")), tag.as_bytes()).await?;
    Ok((StatusCode::OK, [(header::ETAG, tag)]).into_response())
}

async fn finish_upload(s: &SharedState, user: &AuthUser, root: &StorageRoot, bucket: &str, key: &str, id: &str, req: Request) -> S3Result<Response> {
    let dir = upload_dir(root, id)?;
    let rel = tokio::fs::read_to_string(dir.join("key"))
        .await
        .map_err(|_| S3Error::new(StatusCode::NOT_FOUND, "NoSuchUpload", "The specified upload does not exist."))?;
    may_write(user, root, &rel)?;
    let body = read_small_body(req).await?;
    let numbers: Vec<u32> = keys_in(&body, "PartNumber").iter().filter_map(|n| n.trim().parse().ok()).collect();
    if numbers.is_empty() || numbers.windows(2).any(|w| w[0] >= w[1]) {
        return Err(S3Error::new(StatusCode::BAD_REQUEST, "InvalidPartOrder", "List the parts in order"));
    }
    let tmp = dir.join("whole");
    let mut out = tokio::fs::File::create(&tmp).await?;
    for n in &numbers {
        let mut part = tokio::fs::File::open(dir.join(format!("{n:05}.part")))
            .await
            .map_err(|_| S3Error::new(StatusCode::BAD_REQUEST, "InvalidPart", format!("Part {n} was not uploaded")))?;
        tokio::io::copy(&mut part, &mut out).await?;
    }
    out.flush().await?;
    drop(out);
    place(s, root, &rel, &tmp).await?;
    let _ = tokio::fs::remove_dir_all(&dir).await;
    let file = s.db.get_file_by_path(&root.id, &rel).await?.ok_or_else(S3Error::no_key)?;
    Ok(xml(format!(
        "<CompleteMultipartUploadResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Bucket>{}</Bucket><Key>{}</Key><ETag>{}</ETag></CompleteMultipartUploadResult>",
        esc(bucket),
        esc(key),
        esc(&etag(&file))
    )))
}

async fn abort_upload(_s: &SharedState, root: &StorageRoot, id: &str) -> S3Result<Response> {
    let dir = upload_dir(root, id)?;
    let _ = tokio::fs::remove_dir_all(&dir).await;
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The signing example from the AWS documentation ("GET Object").
    #[test]
    fn derives_the_aws_example_signature() {
        let secret = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
        let canonical = "GET\n/test.txt\n\nhost:examplebucket.s3.amazonaws.com\nrange:bytes=0-9\nx-amz-content-sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855\nx-amz-date:20130524T000000Z\n\nhost;range;x-amz-content-sha256;x-amz-date\ne3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        let to_sign = format!("AWS4-HMAC-SHA256\n20130524T000000Z\n20130524/us-east-1/s3/aws4_request\n{}", sha256_hex(canonical.as_bytes()));
        let mut key = hmac_sign(format!("AWS4{secret}").as_bytes(), "20130524");
        for p in ["us-east-1", "s3", "aws4_request"] {
            key = hmac_sign(&key, p);
        }
        assert_eq!(hex(&hmac_sign(&key, &to_sign)), "f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41");
    }

    #[test]
    fn reads_keys_out_of_request_documents() {
        let body = "<Delete><Object><Key>a/b.txt</Key></Object><Object><Key>x &amp; y</Key></Object></Delete>";
        assert_eq!(keys_in(body, "Key"), ["a/b.txt", "x & y"]);
        assert_eq!(bucket_and_key("/photos/2024/a%20b.jpg"), ("photos".into(), "2024/a b.jpg".into()));
    }
}
