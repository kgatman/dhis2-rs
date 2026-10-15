//! `org.hisp.dhis.webapi.filter.ExcludableShallowEtagHeaderFilter` (Spring 7.0.9
//! `ShallowEtagHeaderFilter` minus some streaming endpoints).
//!
//! The response is buffered (so it always gets a `Content-Length`). For GET, 2xx, without
//! `no-store` in `Cache-Control`, an `ETag` of `"0` + MD5 hex of the body + `"` is added unless
//! one is already set, and a matching `If-None-Match` turns the response into a 304.
//! HEAD is not eligible.

use std::sync::LazyLock;

use axum::body::Body;
use axum::extract::Request;
use axum::middleware::Next;
use axum::response::Response;
use http::{HeaderValue, Method, StatusCode, header};
use http_body_util::BodyExt;
use md5::{Digest, Md5};
use regex::Regex;

use super::RequestContext;

const UID: &str = "[a-zA-Z][a-zA-Z0-9]{10}";

/// `ExcludableShallowEtagHeaderFilter.ENDPOINTS`, matched with `find()` against the request URI.
static EXCLUDED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        "/api/(\\d{{2}}/)?dataValueSets|\
         /api/(\\d{{2}}/)?dataValues|\
         /api/(\\d{{2}}/)?fileResources|\
         /api/(\\d{{2}}/)?dataEntry/metadata|\
         /api/(\\d{{2}}/)?tracker/events/{UID}/dataValues/{UID}/(file|image)|\
         /api/(\\d{{2}}/)?tracker/trackedEntities/{UID}/attributes/{UID}/(file|image)|\
         /api/(\\d{{2}}/)?metadata/version/.+/data(\\.gz)?"
    ))
    .expect("valid regex")
});

pub fn etag_for(body: &[u8]) -> String {
    let digest = Md5::digest(body);
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!("\"0{hex}\"")
}

/// `If-None-Match` comparison as in `ServletWebRequest#checkNotModified` (weak comparison).
fn none_match(header: &str, etag: &str) -> bool {
    let strip = |t: &str| t.trim().trim_start_matches("W/").to_string();
    let wanted = strip(etag);
    header
        .split(',')
        .map(str::trim)
        .any(|t| t == "*" || strip(t) == wanted)
}

pub async fn layer(req: Request, next: Next) -> Response {
    let path = req
        .extensions()
        .get::<RequestContext>()
        .map(|c| c.original_path.clone())
        .unwrap_or_else(|| req.uri().path().to_string());
    let excluded = EXCLUDED.is_match(&path);
    let is_get = req.method() == Method::GET;
    let if_none_match = req
        .headers()
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);

    let resp = next.run(req).await;
    if excluded {
        return resp;
    }
    let (mut parts, body) = resp.into_parts();
    let bytes = match body.collect().await {
        Ok(b) => b.to_bytes(),
        Err(e) => {
            tracing::error!("buffering response body: {e}");
            let mut r = Response::new(Body::empty());
            *r.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
            return r;
        }
    };

    let eligible = is_get
        && parts.status.is_success()
        && !parts
            .headers
            .get(header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|cc| cc.contains("no-store"));
    if eligible {
        let etag = match parts
            .headers
            .get(header::ETAG)
            .and_then(|v| v.to_str().ok())
        {
            Some(existing) if !existing.trim().is_empty() => existing.to_string(),
            _ => {
                let e = etag_for(&bytes);
                if let Ok(v) = HeaderValue::from_str(&e) {
                    parts.headers.insert(header::ETAG, v);
                }
                e
            }
        };
        if if_none_match
            .as_deref()
            .is_some_and(|h| none_match(h, &etag))
        {
            parts.status = StatusCode::NOT_MODIFIED;
            parts.headers.remove(header::CONTENT_LENGTH);
            return Response::from_parts(parts, Body::empty());
        }
    }
    parts.headers.remove(header::CONTENT_LENGTH);
    Response::from_parts(parts, Body::from(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn etag_format_matches_spring() {
        // Spring: "0" + md5 hex; measured on /api/system/uid CSV output.
        assert_eq!(etag_for(b""), "\"0d41d8cd98f00b204e9800998ecf8427e\"");
    }

    #[test]
    fn if_none_match_parsing() {
        let e = "\"0abc\"";
        assert!(none_match("\"0abc\"", e));
        assert!(none_match("W/\"0abc\"", e));
        assert!(none_match("\"x\", \"0abc\"", e));
        assert!(none_match("*", e));
        assert!(!none_match("\"0abd\"", e));
    }

    #[test]
    fn exclusions() {
        assert!(EXCLUDED.is_match("/api/dataValueSets"));
        assert!(EXCLUDED.is_match("/api/41/dataValues/files"));
        assert!(!EXCLUDED.is_match("/api/system/uid"));
    }
}
