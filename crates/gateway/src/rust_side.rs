//! Running a request through the in-process Rust router.
//!
//! 1. The path is normalised the way Java normalises it before handler lookup (version prefix,
//!    trailing slash, registered extension); the original path and stripped extension travel in
//!    [`RequestContext`].
//! 2. The request is authenticated by Java (`dhis_auth::delegated`), unless disabled.
//! 3. A handler may answer [`JavaFallback`]; the caller then replays the original request to
//!    Java (ADR 0004).
//! 4. `Set-Cookie` headers Java emitted while authenticating are relayed on the Rust response.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use dhis_auth::delegated::{AuthOutcome, JavaAuthenticator};
use dhis_web::http::{JavaFallback, RequestContext};
use http::{HeaderMap, Method, Request, Response, Uri, Version, header};
use tower::ServiceExt;
use tracing::{debug, error, info};

use crate::identity::IdentityTracker;
use crate::routes;

/// The parts of a request needed to run it again (on Rust, or on Java after a fallback).
#[derive(Debug, Clone)]
pub struct RequestHead {
    pub method: Method,
    pub uri: Uri,
    pub version: Version,
    pub headers: HeaderMap,
}

impl RequestHead {
    pub fn from_parts(p: &http::request::Parts) -> Self {
        Self {
            method: p.method.clone(),
            uri: p.uri.clone(),
            version: p.version,
            headers: p.headers.clone(),
        }
    }

    pub fn to_request<B>(&self, body: B) -> Request<B> {
        let mut r = Request::new(body);
        *r.method_mut() = self.method.clone();
        *r.uri_mut() = self.uri.clone();
        *r.version_mut() = self.version;
        *r.headers_mut() = self.headers.clone();
        r
    }
}

pub enum RustOutcome {
    Response(Response<axum::body::Body>),
    /// The handler asked for Java to answer.
    Fallback(&'static str),
    /// Java did not authenticate the request; Java should answer it (302/401/...).
    NotAuthenticated,
    /// Authentication could not be checked (Java unreachable).
    Unavailable,
}

pub struct RustSide {
    router: axum::Router,
    auth: Option<JavaAuthenticator>,
    identity: Option<Arc<IdentityTracker>>,
    fallbacks: Mutex<BTreeMap<&'static str, u64>>,
}

impl RustSide {
    pub fn new(
        router: axum::Router,
        auth: Option<JavaAuthenticator>,
        identity: Option<Arc<IdentityTracker>>,
    ) -> Self {
        Self {
            router,
            auth,
            identity,
            fallbacks: Mutex::new(BTreeMap::new()),
        }
    }

    /// Fallbacks to Java so far, by reason. At cutover this must be empty.
    pub fn fallback_counts(&self) -> BTreeMap<&'static str, u64> {
        self.fallbacks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub async fn run(&self, head: &RequestHead, body: Bytes) -> RustOutcome {
        let (normalized, extension) = routes::normalize(head.uri.path());
        let probe = dhis_web::auth_probe(&normalized);
        let normalized_path = normalized.clone();
        let path_and_query = match head.uri.query() {
            Some(q) => format!("{normalized}?{q}"),
            None => normalized,
        };
        let Ok(uri) = path_and_query.parse::<Uri>() else {
            return RustOutcome::Fallback("unparseable path");
        };
        let mut req = head.to_request(axum::body::Body::from(body));
        *req.uri_mut() = uri;
        req.extensions_mut().insert(RequestContext {
            original_path: head.uri.path().to_string(),
            extension,
        });

        let mut set_cookies = Vec::new();
        if let Some(auth) = &self.auth {
            match auth.authenticate(&head.headers, probe).await {
                AuthOutcome::Authenticated {
                    mut user,
                    set_cookies: c,
                } => {
                    if let Some(tracker) = &self.identity
                        && dhis_web::needs_identity(&normalized_path)
                    {
                        user.identity = tracker.resolve(&head.headers, &c).await;
                    }
                    req.extensions_mut().insert(user);
                    set_cookies = c;
                }
                AuthOutcome::NotAuthenticated { .. } => return RustOutcome::NotAuthenticated,
                AuthOutcome::Unavailable(e) => {
                    error!("delegated authentication failed: {e}");
                    return RustOutcome::Unavailable;
                }
            }
        }

        let resp = match self.router.clone().oneshot(req).await {
            Ok(r) => r,
            Err(never) => match never {},
        };
        if let Some(fb) = resp.extensions().get::<JavaFallback>() {
            *self
                .fallbacks
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .entry(fb.reason)
                .or_default() += 1;
            info!(method = %head.method, path = %head.uri.path(), reason = fb.reason, "fallback to Java");
            // the query can carry values worth keeping out of normal logs
            debug!(
                query = head.uri.query().unwrap_or(""),
                reason = fb.reason,
                "fallback query"
            );
            return RustOutcome::Fallback(fb.reason);
        }
        let mut resp = resp;
        for c in set_cookies {
            resp.headers_mut().append(header::SET_COOKIE, c);
        }
        RustOutcome::Response(resp)
    }
}
