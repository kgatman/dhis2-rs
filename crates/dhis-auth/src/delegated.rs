//! Authentication delegated to the Java server (charter §2, "shared auth during transition").
//!
//! For each request routed to Rust, the client's credentials (all request headers minus the
//! body-related and hop-by-hop ones) are replayed to Java's `GET /api/me`. Java then does exactly
//! what it does for any API request: authenticates (session cookie, basic auth, PAT, OAuth2
//! bearer, ...), creates or touches the session and emits its `Set-Cookie` headers
//! (`JSESSIONID` for new sessions, `SESSION_EXPIRE` from `SessionTimeoutHeaderFilter`), and logs
//! the authentication event. Those `Set-Cookie` headers are relayed on the Rust response, so the
//! client sees the same cookies as from Java.
//!
//! No caching: a cache would skip Java's per-request session side effects (new session per
//! basic-auth request, session expiry refresh). Revisit with native auth (phase 7).

use bytes::Bytes;
use http::{HeaderMap, HeaderValue, Method, Request, StatusCode, Uri, header};
use http_body_util::{BodyExt, Empty};
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;

use crate::CurrentUser;

/// Fields requested from `/api/me`; enough for authority checks.
pub const ME_FIELDS: &str = "id,username,authorities";

#[derive(Debug)]
pub enum AuthOutcome {
    Authenticated {
        user: CurrentUser,
        /// `Set-Cookie` headers Java emitted while authenticating; relay them to the client.
        set_cookies: Vec<HeaderValue>,
    },
    /// Java did not authenticate the request (it would answer 302/401/403). Let Java answer
    /// the original request itself.
    NotAuthenticated { status: StatusCode },
    /// Java unreachable or answered something unexpected.
    Unavailable(String),
}

pub struct JavaAuthenticator {
    java: Uri,
    client: Client<HttpConnector, Empty<Bytes>>,
}

const DROPPED: &[&str] = &[
    "content-length",
    "content-type",
    "content-encoding",
    "transfer-encoding",
    "expect",
    "connection",
    "keep-alive",
    "te",
    "trailer",
    "upgrade",
    "proxy-connection",
    "accept",
    "if-none-match",
    "if-modified-since",
    "if-match",
    "if-unmodified-since",
    "range",
    "origin",
];

impl JavaAuthenticator {
    /// `java`: scheme and authority of the Java server, e.g. `http://web:8080`.
    pub fn new(java: Uri) -> Self {
        Self {
            java,
            client: Client::builder(TokioExecutor::new()).build(HttpConnector::new()),
        }
    }

    pub async fn authenticate(&self, client_headers: &HeaderMap) -> AuthOutcome {
        let uri = format!(
            "{}://{}/api/me?fields={ME_FIELDS}",
            self.java.scheme_str().unwrap_or("http"),
            self.java
                .authority()
                .map(|a| a.as_str())
                .unwrap_or_default()
        );
        let mut req = match Request::builder()
            .method(Method::GET)
            .uri(uri)
            .body(Empty::<Bytes>::new())
        {
            Ok(r) => r,
            Err(e) => return AuthOutcome::Unavailable(e.to_string()),
        };
        let headers = req.headers_mut();
        for (k, v) in client_headers {
            if !DROPPED.contains(&k.as_str()) {
                headers.append(k.clone(), v.clone());
            }
        }
        headers.insert(header::ACCEPT, HeaderValue::from_static("application/json"));

        let resp = match self.client.request(req).await {
            Ok(r) => r,
            Err(e) => return AuthOutcome::Unavailable(e.to_string()),
        };
        let status = resp.status();
        let set_cookies: Vec<HeaderValue> = resp
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .cloned()
            .collect();
        if status != StatusCode::OK {
            return AuthOutcome::NotAuthenticated { status };
        }
        let body = match resp.into_body().collect().await {
            Ok(b) => b.to_bytes(),
            Err(e) => return AuthOutcome::Unavailable(e.to_string()),
        };
        match serde_json::from_slice::<CurrentUser>(&body) {
            Ok(user) => AuthOutcome::Authenticated { user, set_cookies },
            Err(e) => AuthOutcome::Unavailable(format!("unexpected /api/me body: {e}")),
        }
    }
}
