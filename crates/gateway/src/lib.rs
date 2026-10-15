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

pub mod routes;

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
use tower::ServiceExt;
use tracing::{debug, error, warn};

pub use routes::{Backend, RouteTable};

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
}

pub struct Gateway {
    config: Config,
    routes: RwLock<Arc<RouteTable>>,
    client: Client<HttpConnector, Incoming>,
    rust: axum::Router,
}

impl Gateway {
    pub fn new(config: Config, routes: RouteTable, rust: axum::Router) -> Self {
        let mut connector = HttpConnector::new();
        connector.set_nodelay(true);
        let client = Client::builder(TokioExecutor::new())
            .http1_preserve_header_case(true)
            .http1_title_case_headers(true)
            .build(connector);
        Self {
            config,
            routes: RwLock::new(Arc::new(routes)),
            client,
            rust,
        }
    }

    /// Swaps the route table (used on `SIGHUP`).
    pub fn set_routes(&self, routes: RouteTable) {
        *self.routes.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(routes);
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
        let (backend, _) = self.routes().route(&method, &path);
        let resp = match backend {
            Backend::Java => self.to_java(req, peer).await,
            Backend::Rust => self.to_rust(req).await,
        };
        debug!(
            %method, %path, ?backend, status = resp.status().as_u16(),
            elapsed_ms = start.elapsed().as_millis() as u64, "request"
        );
        resp
    }

    async fn to_java(&self, req: Request<Incoming>, peer: SocketAddr) -> Response<Body> {
        let (mut parts, body) = req.into_parts();

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
                Response::from_parts(parts, body.map_err(Into::into).boxed_unsync())
            }
            Err(e) => {
                error!("upstream request failed: {e}");
                status_only(StatusCode::BAD_GATEWAY)
            }
        }
    }

    async fn to_rust(&self, req: Request<Incoming>) -> Response<Body> {
        let req = req.map(axum::body::Body::new);
        match self.rust.clone().oneshot(req).await {
            Ok(resp) => resp.map(|b| b.map_err(Into::into).boxed_unsync()),
            Err(never) => match never {},
        }
    }
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
