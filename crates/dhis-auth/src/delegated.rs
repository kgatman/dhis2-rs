//! Authentication delegated to the Java server (charter §2, "shared auth during transition").
//!
//! For each request routed to Rust, the client's credentials (all request headers minus the
//! body-related and hop-by-hop ones) are replayed to a cheap Java endpoint ([`Probe`]). Java then
//! does exactly what it does for any API request: authenticates (session cookie, basic auth, PAT, OAuth2
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

use crate::{CurrentUser, JsonMembers};

/// What the authentication call asks Java for. Both are cheap `MeController` endpoints answered
/// from the session (`/api/me` itself builds the whole `MeDto`: measured 2.5–8 ms against
/// 0.4–0.7 ms), and both go through the same security filter chain, so authentication, session
/// and cookie side effects are those of any API request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Probe {
    /// `GET /api/me/authorities` → [`CurrentUser::authorities`].
    Authorities,
    /// `GET /api/me/settings` → [`CurrentUser::settings`].
    Settings,
}

impl Probe {
    fn path(self) -> &'static str {
        match self {
            Probe::Authorities => "/api/me/authorities",
            Probe::Settings => "/api/me/settings",
        }
    }
}

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

    /// `GET path_and_query` on Java with the client's credentials, accepting JSON.
    async fn get(
        &self,
        path_and_query: &str,
        client_headers: &HeaderMap,
    ) -> Result<http::Response<hyper::body::Incoming>, String> {
        let uri = format!(
            "{}://{}{path_and_query}",
            self.java.scheme_str().unwrap_or("http"),
            self.java
                .authority()
                .map(|a| a.as_str())
                .unwrap_or_default()
        );
        let mut req = Request::builder()
            .method(Method::GET)
            .uri(uri)
            .body(Empty::<Bytes>::new())
            .map_err(|e| e.to_string())?;
        let headers = req.headers_mut();
        for (k, v) in client_headers {
            if !DROPPED.contains(&k.as_str()) {
                headers.append(k.clone(), v.clone());
            }
        }
        headers.insert(header::ACCEPT, HeaderValue::from_static("application/json"));
        self.client.request(req).await.map_err(|e| e.to_string())
    }

    /// Reads a JSON document from Java as the requesting user, to mirror values only Java knows
    /// (e.g. its JVM details, see ADR 0005). `None` unless Java answers 200 with JSON.
    pub async fn fetch_json(
        &self,
        path_and_query: &str,
        client_headers: &HeaderMap,
    ) -> Option<serde_json::Value> {
        let resp = self.get(path_and_query, client_headers).await.ok()?;
        if resp.status() != StatusCode::OK {
            return None;
        }
        let body = resp.into_body().collect().await.ok()?.to_bytes();
        serde_json::from_slice(&body).ok()
    }

    pub async fn authenticate(&self, client_headers: &HeaderMap, probe: Probe) -> AuthOutcome {
        let resp = match self.get(probe.path(), client_headers).await {
            Ok(r) => r,
            Err(e) => return AuthOutcome::Unavailable(e),
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
        let user = match probe {
            Probe::Authorities => {
                serde_json::from_slice::<Vec<String>>(&body).map(|a| CurrentUser {
                    authorities: Some(a),
                    settings: None,
                })
            }
            Probe::Settings => serde_json::from_slice::<JsonMembers>(&body).map(|s| CurrentUser {
                authorities: None,
                settings: Some(s),
            }),
        };
        match user {
            Ok(user) => AuthOutcome::Authenticated { user, set_cookies },
            Err(e) => AuthOutcome::Unavailable(format!("unexpected {} body: {e}", probe.path())),
        }
    }
}
