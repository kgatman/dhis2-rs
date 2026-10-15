//! Strangler-fig gateway in front of the Java DHIS2 server.
//!
//! Every request goes to Java unless the route table ([`routes`]) sends it to the in-process Rust
//! router (`dhis_web::router()`). Rolling back an endpoint is a route-table edit plus `SIGHUP`
//! (or restart).
//!
//! The proxy aims to be invisible to both sides:
//! - method, path, query, body and headers pass through unchanged, including `Host` and the
//!   original header-name casing (HTTP/1);
//! - hop-by-hop headers (RFC 9110 §7.6.1) and `Expect` are dropped, as any proxy must;
//! - nothing is added to responses; `X-Forwarded-For` is only added to requests when the client
//!   did not send one (configurable, see [`Config::inject_forwarded_for`]);
//! - bodies stream in both directions (no buffering), so large exports and imports behave the
//!   same.
//!
//! `shadow` routes (GET/HEAD only) are served by Java while the request is replayed to Rust in the
//! background; the responses are compared with diffcheck's rules and logged ([`shadow`]).
//!
//! Rust requests are authenticated by Java and may fall back to Java ([`rust_side`], ADR 0004).

pub mod firewall;
pub mod routes;
pub mod rust_side;
mod shadow;
pub mod tomcat;

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{Arc, RwLock};
use std::time::Instant;

use bytes::Bytes;
use http::header::{self, HeaderMap, HeaderName, HeaderValue};
use http::{Request, Response, StatusCode, Uri};
use http_body_util::BodyExt;
use http_body_util::combinators::UnsyncBoxBody;
use hyper::body::Incoming;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::{TokioExecutor, TokioIo};
use tokio::net::TcpListener;
use tracing::{debug, error, warn};

pub use routes::{Backend, RouteTable};
pub use rust_side::{RequestHead, RustOutcome, RustSide};
pub use shadow::{ShadowConfig, ShadowStats};

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;
pub type Body = UnsyncBoxBody<Bytes, BoxError>;

#[derive(Debug, Clone)]
pub struct Config {
    /// Java base URL, scheme + authority only (e.g. `http://web:8080`). The request path is
    /// forwarded as-is, so a Java context path (e.g. `/dhis`) must be part of client URLs.
    pub java_upstream: Uri,
    /// Add `X-Forwarded-For: <peer ip>` when the client sent none, so Java code that prefers that
    /// header (API-token IP allowlists, auth audit) still sees the client's address. Turn off
    /// when the gateway's peer is not the real client (e.g. sharing a network namespace with it).
    pub inject_forwarded_for: bool,
    pub shadow: ShadowConfig,
    /// Authenticate Rust-routed requests through Java's `/api/me` (phases 1–6). Off only in
    /// tests and for routers without authenticated endpoints.
    pub delegate_auth: bool,
}

pub struct Gateway {
    config: Config,
    routes: RwLock<Arc<RouteTable>>,
    client: Client<HttpConnector, Body>,
    rust: Arc<RustSide>,
    shadow: Arc<shadow::Shadow>,
}

impl Gateway {
    pub fn new(config: Config, routes: RouteTable, rust: axum::Router) -> Self {
        let mut connector = HttpConnector::new();
        connector.set_nodelay(true);
        let client = Client::builder(TokioExecutor::new())
            .http1_preserve_header_case(true)
            .http1_title_case_headers(true)
            .build(connector);
        let shadow = Arc::new(shadow::Shadow::new(config.shadow.clone()));
        let auth = config
            .delegate_auth
            .then(|| dhis_auth::delegated::JavaAuthenticator::new(config.java_upstream.clone()));
        Self {
            rust: Arc::new(RustSide::new(rust, auth)),
            config,
            routes: RwLock::new(Arc::new(routes)),
            client,
            shadow,
        }
    }

    /// Swaps the route table (used on `SIGHUP`).
    pub fn set_routes(&self, routes: RouteTable) {
        *self.routes.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(routes);
    }

    /// Requests the Rust side handed back to Java, by reason.
    pub fn fallback_counts(&self) -> std::collections::BTreeMap<&'static str, u64> {
        self.rust.fallback_counts()
    }

    pub fn shadow_stats(&self) -> ShadowStats {
        self.shadow.stats()
    }

    pub fn routes(&self) -> Arc<RouteTable> {
        self.routes
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Accepts connections until the listener fails.
    pub async fn serve(self: Arc<Self>, listener: TcpListener) -> std::io::Result<()> {
        loop {
            let (stream, peer) = listener.accept().await?;
            let _ = stream.set_nodelay(true);
            let gw = self.clone();
            tokio::spawn(async move {
                let svc = hyper::service::service_fn(move |req| {
                    let gw = gw.clone();
                    async move { Ok::<_, Infallible>(gw.handle(req, peer).await) }
                });
                let mut builder =
                    hyper_util::server::conn::auto::Builder::new(TokioExecutor::new());
                builder
                    .http1()
                    .preserve_header_case(true)
                    .title_case_headers(true);
                if let Err(e) = builder.serve_connection(TokioIo::new(stream), svc).await {
                    debug!(%peer, "connection ended: {e}");
                }
            });
        }
    }

    pub async fn handle(&self, req: Request<Incoming>, peer: SocketAddr) -> Response<Body> {
        let start = Instant::now();
        let method = req.method().clone();
        let path = req.uri().path().to_string();
        let (mut backend, _) = self.routes().route(&method, &path);
        // Requests Tomcat or Spring Security's firewall reject (400) never reach a controller;
        // let Java answer them.
        let target = req.uri().path_and_query().map_or("/", |p| p.as_str());
        if backend != Backend::Java
            && (!tomcat::is_valid_request_target(target)
                || tomcat::has_parameter_decode_failure(req.uri().query())
                || tomcat::has_malformed_path_escape(req.uri().path())
                || firewall::rejects(req.uri().path()))
        {
            backend = Backend::Java;
        }
        let resp = match backend {
            Backend::Java => self.to_java(req.map(box_incoming), peer).await,
            Backend::Rust => self.to_rust(req, peer).await,
            Backend::Shadow => self.to_shadow(req, peer).await,
        };
        debug!(
            %method, %path, ?backend, status = resp.status().as_u16(),
            elapsed_ms = start.elapsed().as_millis() as u64, "request"
        );
        resp
    }

    async fn to_java(&self, req: Request<Body>, peer: SocketAddr) -> Response<Body> {
        let (mut parts, body) = req.into_parts();
        let client_version = parts.version;

        let path_and_query = parts
            .uri
            .path_and_query()
            .map(|p| p.as_str())
            .unwrap_or("/");
        let upstream = format!(
            "{}://{}{}",
            self.config.java_upstream.scheme_str().unwrap_or("http"),
            self.config
                .java_upstream
                .authority()
                .map(|a| a.as_str())
                .unwrap_or_default(),
            path_and_query
        );
        let original_authority = parts.uri.authority().cloned();
        parts.uri = match upstream.parse() {
            Ok(u) => u,
            Err(e) => {
                warn!("cannot build upstream URI {upstream}: {e}");
                return status_only(StatusCode::BAD_REQUEST);
            }
        };

        // HTTP/2 clients send :authority instead of Host; Java needs a Host header.
        if !parts.headers.contains_key(header::HOST)
            && let Some(a) = original_authority
            && let Ok(v) = HeaderValue::from_str(a.as_str())
        {
            parts.headers.insert(header::HOST, v);
        }
        parts.version = http::Version::HTTP_11;
        strip_hop_by_hop(&mut parts.headers);
        parts.headers.remove(header::EXPECT);
        if self.config.inject_forwarded_for
            && !parts.headers.contains_key("x-forwarded-for")
            && let Ok(v) = HeaderValue::from_str(&peer.ip().to_string())
        {
            parts.headers.insert("x-forwarded-for", v);
        }

        match self.client.request(Request::from_parts(parts, body)).await {
            Ok(resp) => {
                let (mut parts, body) = resp.into_parts();
                strip_hop_by_hop(&mut parts.headers);
                // Answer in the client's protocol version, not the upstream's.
                parts.version = client_version;
                Response::from_parts(parts, body.map_err(Into::into).boxed_unsync())
            }
            Err(e) => {
                error!("upstream request failed: {e}");
                status_only(StatusCode::BAD_GATEWAY)
            }
        }
    }

    async fn to_rust(&self, req: Request<Incoming>, peer: SocketAddr) -> Response<Body> {
        let (parts, body) = req.into_parts();
        // Buffered so the request can be replayed to Java on fallback.
        let body = match body.collect().await {
            Ok(b) => b.to_bytes(),
            Err(e) => {
                debug!("client body read failed: {e}");
                return status_only(StatusCode::BAD_REQUEST);
            }
        };
        let head = RequestHead::from_parts(&parts);
        match self.rust.run(&head, body.clone()).await {
            RustOutcome::Response(mut r) => {
                // Tomcat writes no reason phrase ("HTTP/1.1 200 "); match it.
                r.extensions_mut()
                    .insert(hyper::ext::ReasonPhrase::from_static(b""));
                r.map(|b| b.map_err(Into::into).boxed_unsync())
            }
            RustOutcome::Fallback(_) | RustOutcome::NotAuthenticated => {
                // `parts` keeps the original extensions (header-case map) for Java.
                self.to_java(Request::from_parts(parts, full(body)), peer)
                    .await
            }
            RustOutcome::Unavailable => status_only(StatusCode::BAD_GATEWAY),
        }
    }

    /// Serves Java's response to the client and compares Rust's in the background.
    async fn to_shadow(&self, req: Request<Incoming>, peer: SocketAddr) -> Response<Body> {
        let (parts, body) = req.into_parts();
        // GET/HEAD bodies are empty in practice; buffer so both backends get the same bytes.
        let body = match body.collect().await {
            Ok(b) => b.to_bytes(),
            Err(e) => {
                debug!("client body read failed: {e}");
                return status_only(StatusCode::BAD_REQUEST);
            }
        };
        let head = RequestHead::from_parts(&parts);
        let rust_body = body.clone();
        let method = parts.method.clone();
        let path_and_query = parts
            .uri
            .path_and_query()
            .map(|p| p.to_string())
            .unwrap_or_else(|| "/".into());

        let java_req = Request::from_parts(parts, full(body));
        let java = self.to_java(java_req, peer).await;
        let (java_parts, java_body) = java.into_parts();
        let (tee, java_capture) = shadow::Tee::new(java_body, self.shadow.body_limit());

        let shadow = self.shadow.clone();
        let rust = self.rust.clone();
        let java_status = java_parts.status;
        let java_headers = java_parts.headers.clone();
        // The Rust side runs off the request path: the client only waits for Java.
        tokio::spawn(async move {
            match rust.run(&head, rust_body).await {
                RustOutcome::Response(rust_resp) => {
                    shadow
                        .compare(
                            method,
                            path_and_query,
                            java_status,
                            java_headers,
                            java_capture,
                            rust_resp,
                        )
                        .await
                }
                RustOutcome::Fallback(reason) => shadow.skip(&method, &path_and_query, reason),
                RustOutcome::NotAuthenticated => {
                    shadow.skip(&method, &path_and_query, "not authenticated")
                }
                RustOutcome::Unavailable => {
                    shadow.skip(&method, &path_and_query, "auth unavailable")
                }
            }
        });
        Response::from_parts(java_parts, tee.boxed_unsync())
    }
}

fn box_incoming(b: Incoming) -> Body {
    b.map_err(Into::into).boxed_unsync()
}

fn full(b: Bytes) -> Body {
    http_body_util::Full::new(b)
        .map_err(|never| match never {})
        .boxed_unsync()
}

fn status_only(status: StatusCode) -> Response<Body> {
    let mut r = Response::new(
        http_body_util::Empty::<Bytes>::new()
            .map_err(|never| match never {})
            .boxed_unsync(),
    );
    *r.status_mut() = status;
    r
}

/// RFC 9110 §7.6.1: hop-by-hop fields, plus any named in `Connection`.
pub fn strip_hop_by_hop(headers: &mut HeaderMap) {
    let listed: Vec<HeaderName> = headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .filter_map(|n| HeaderName::from_bytes(n.trim().as_bytes()).ok())
        .collect();
    for name in listed {
        headers.remove(name);
    }
    for name in [
        "connection",
        "keep-alive",
        "proxy-connection",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
    ] {
        headers.remove(name);
    }
}
