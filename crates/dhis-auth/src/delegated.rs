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
    /// Both, one after the other: the authorities call authenticates; the settings call then
    /// rides on the session it created or used (`JSESSIONID`), so Java sees one authentication
    /// (one new session per basic-auth request, as for a request Java answers itself). Without a
    /// session to ride on (e.g. a token that creates none), settings stay `None`.
    AuthoritiesAndSettings,
}

impl Probe {
    fn path(self) -> &'static str {
        match self {
            Probe::Authorities | Probe::AuthoritiesAndSettings => "/api/me/authorities",
            Probe::Settings => "/api/me/settings",
        }
    }
}

/// Tomcat's session cookie (DHIS2 keeps the default name; measured).
const SESSION_COOKIE: &str = "JSESSIONID";

/// Headers for a follow-up call on the session of the first call: the client's headers with the
/// cookies Java just set (as a browser would apply them) and without credentials when a new
/// session was created. `None` when there is no session to use.
fn follow_up_headers(client: &HeaderMap, set_cookies: &[HeaderValue]) -> Option<HeaderMap> {
    let new: Vec<(String, String)> = set_cookies
        .iter()
        .filter_map(|v| v.to_str().ok())
        .filter_map(|v| v.split(';').next())
        .filter_map(|nv| nv.split_once('='))
        .map(|(n, v)| (n.trim().to_string(), v.trim().to_string()))
        .collect();
    let new_session = new.iter().any(|(n, _)| n == SESSION_COOKIE);
    let has_credentials = client.contains_key(header::AUTHORIZATION);
    if !new_session && has_credentials {
        // stateless credentials (a token): another call would authenticate again
        return None;
    }
    let mut cookies: Vec<(String, String)> = client
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|nv| nv.split_once('='))
        .map(|(n, v)| (n.trim().to_string(), v.trim().to_string()))
        .collect();
    if !new_session && !cookies.iter().any(|(n, _)| n == SESSION_COOKIE) {
        return None;
    }
    for (n, v) in new {
        cookies.retain(|(c, _)| *c != n);
        cookies.push((n, v));
    }
    let mut headers = client.clone();
    headers.remove(header::AUTHORIZATION);
    headers.remove(header::COOKIE);
    let cookie = cookies
        .iter()
        .map(|(n, v)| format!("{n}={v}"))
        .collect::<Vec<_>>()
        .join("; ");
    headers.insert(header::COOKIE, HeaderValue::from_str(&cookie).ok()?);
    Some(headers)
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
        // A request on an existing session (cookie, no credentials) needs no session from the
        // first call: both calls run at once.
        let on_session = probe == Probe::AuthoritiesAndSettings
            && !client_headers.contains_key(header::AUTHORIZATION)
            && follow_up_headers(client_headers, &[]).is_some();
        if on_session {
            let (outcome, settings) = tokio::join!(
                self.authenticate_once(client_headers, Probe::Authorities),
                self.fetch_settings(client_headers)
            );
            return match outcome {
                AuthOutcome::Authenticated {
                    mut user,
                    set_cookies,
                } => {
                    user.settings = settings;
                    AuthOutcome::Authenticated { user, set_cookies }
                }
                other => other,
            };
        }
        self.authenticate_once(client_headers, probe).await
    }

    async fn authenticate_once(&self, client_headers: &HeaderMap, probe: Probe) -> AuthOutcome {
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
            Probe::Authorities | Probe::AuthoritiesAndSettings => {
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
        let mut user = match user {
            Ok(user) => user,
            Err(e) => {
                return AuthOutcome::Unavailable(format!("unexpected {} body: {e}", probe.path()));
            }
        };
        if probe == Probe::AuthoritiesAndSettings
            && let Some(headers) = follow_up_headers(client_headers, &set_cookies)
        {
            user.settings = self.fetch_settings(&headers).await;
        }
        AuthOutcome::Authenticated { user, set_cookies }
    }

    async fn fetch_settings(&self, headers: &HeaderMap) -> Option<JsonMembers> {
        let resp = self.get(Probe::Settings.path(), headers).await.ok()?;
        if resp.status() != StatusCode::OK {
            return None;
        }
        let body = resp.into_body().collect().await.ok()?.to_bytes();
        serde_json::from_slice(&body).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut m = HeaderMap::new();
        for (k, v) in pairs {
            m.append(
                http::header::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                v.parse().unwrap(),
            );
        }
        m
    }

    #[test]
    fn follow_up_on_new_basic_auth_session() {
        let client = h(&[
            ("authorization", "Basic YWRtaW46ZGlzdHJpY3Q="),
            ("cookie", "a=1"),
        ]);
        let set = [
            HeaderValue::from_static("JSESSIONID=ABC; Path=/; HttpOnly; SameSite=Lax"),
            HeaderValue::from_static("SESSION_EXPIRE=server_time=1&expiry_time=2; Path=/"),
        ];
        let f = follow_up_headers(&client, &set).unwrap();
        assert!(!f.contains_key(header::AUTHORIZATION));
        assert_eq!(
            f.get(header::COOKIE).unwrap(),
            "a=1; JSESSIONID=ABC; SESSION_EXPIRE=server_time=1&expiry_time=2"
        );
    }

    #[test]
    fn follow_up_on_existing_session_or_none() {
        let cookie = h(&[("cookie", "JSESSIONID=OLD")]);
        assert_eq!(
            follow_up_headers(&cookie, &[])
                .unwrap()
                .get(header::COOKIE)
                .unwrap(),
            "JSESSIONID=OLD"
        );
        // a token without a session: another call would authenticate again
        assert!(follow_up_headers(&h(&[("authorization", "ApiToken d2p_x")]), &[]).is_none());
        // no credentials, no session
        assert!(follow_up_headers(&h(&[]), &[]).is_none());
    }
}
