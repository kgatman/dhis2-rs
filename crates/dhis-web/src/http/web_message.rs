//! `org.hisp.dhis.dxf2.webmessage.WebMessage`, the error/status envelope of the Web API, as
//! written by `WebMessageControllerAdvice` and `CrudControllerAdvice`.

use axum::body::Body;
use axum::response::{IntoResponse, Response};
use http::{HeaderValue, StatusCode, header};
use serde::Serialize;

/// `org.hisp.dhis.feedback.Status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Status {
    Ok,
    Warning,
    Error,
}

/// Field order as `@JsonPropertyOrder` on `WebMessage`; null fields are omitted.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebMessage {
    /// Spring's `HttpStatus#getReasonPhrase()`, e.g. `"Bad Request"`.
    pub http_status: String,
    pub http_status_code: u16,
    pub status: Status,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dev_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response: Option<serde_json::Value>,
}

/// Spring's `HttpStatus` reason phrases (they differ from IANA/hyper in a few places).
pub fn spring_reason_phrase(status: StatusCode) -> &'static str {
    match status.as_u16() {
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        406 => "Not Acceptable",
        409 => "Conflict",
        413 => "Payload Too Large",
        415 => "Unsupported Media Type",
        422 => "Unprocessable Entity",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => status.canonical_reason().unwrap_or(""),
    }
}

impl WebMessage {
    pub fn error(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            http_status: spring_reason_phrase(status).to_string(),
            http_status_code: status.as_u16(),
            status: Status::Error,
            code: None,
            message: Some(message.into()),
            dev_message: None,
            response: None,
        }
    }

    /// `WebMessageUtils.badRequest(message)`.
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::error(StatusCode::BAD_REQUEST, message)
    }
}

impl IntoResponse for WebMessage {
    fn into_response(self) -> Response {
        let status = StatusCode::from_u16(self.http_status_code)
            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let body = dhis_core_model::java::json::to_vec(&self);
        let mut resp = Response::new(Body::from(body));
        *resp.status_mut() = status;
        // Written by the Jackson converter (@ResponseBody of the controller advice).
        resp.extensions_mut().insert(super::rfd::ConverterWritten);
        // Measured: WebMessage bodies are `application/json` without a charset (unlike other
        // JSON responses).
        resp.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        // WebMessageControllerAdvice: 4xx/5xx get `no-cache, private` unless already set.
        if status.is_client_error() || status.is_server_error() {
            resp.headers_mut().insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static("no-cache, private"),
            );
        }
        resp
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialises_like_java() {
        let m = WebMessage::bad_request(
            "Value 'abc' is not valid for parameter limit. For input string: \"abc\"",
        );
        assert_eq!(
            dhis_core_model::java::json::to_string(&m),
            r#"{"httpStatus":"Bad Request","httpStatusCode":400,"status":"ERROR","message":"Value 'abc' is not valid for parameter limit. For input string: \"abc\""}"#
        );
    }
}
