//! Cross-cutting HTTP behaviour of the Java Web API, ported once and shared by all handlers.

pub mod etag;
pub mod negotiation;
pub mod paths;
pub mod rfd;
pub mod security_headers;
pub mod web_message;

use axum::response::{IntoResponse, Response};

/// Marker a handler returns when it cannot produce Java's exact response for this request
/// (an unported branch, an unsupported `Accept`, a CORS request, ...). The gateway then replays
/// the original request to Java. See docs/adr/0004-java-fallback.md.
#[derive(Debug, Clone, Copy)]
pub struct JavaFallback {
    /// Short reason, logged and counted by the gateway.
    pub reason: &'static str,
}

impl IntoResponse for JavaFallback {
    fn into_response(self) -> Response {
        let mut r = http::StatusCode::NOT_IMPLEMENTED.into_response();
        r.extensions_mut().insert(self);
        r
    }
}

pub fn fallback(reason: &'static str) -> Response {
    JavaFallback { reason }.into_response()
}

/// A JSON `@ResponseBody` as Spring's Jackson converter writes it: `application/json;charset=UTF-8`,
/// marked for the reflected-file-download check ([`rfd`]).
pub fn json_response(body: impl Into<axum::body::Body>) -> Response {
    let mut r = Response::new(body.into());
    r.headers_mut().insert(
        http::header::CONTENT_TYPE,
        http::HeaderValue::from_static("application/json;charset=UTF-8"),
    );
    r.extensions_mut().insert(rfd::ConverterWritten);
    r
}

/// What the gateway knows about the request before Java-style normalisation; inserted as a
/// request extension.
#[derive(Debug, Clone, Default)]
pub struct RequestContext {
    /// Path as the client sent it (e.g. `/api/41/system/uid.json/`).
    pub original_path: String,
    /// Registered media-type extension that was stripped (e.g. `json`, `adx.xml`).
    pub extension: Option<String>,
}
