//! Differential oracle.
//!
//! Replays each request of a corpus against the Java server and the Rust server (both on the same
//! DB snapshot) and compares status, selected headers and body after normalisation. An endpoint
//! counts as ported only when its whole corpus passes.
//!
//! Normalisation is driven by a rules file in which every rule carries a `reason`; rules without
//! one are rejected, so nothing gets ignored silently.
//!
//! Not done yet (see PORT_STATUS.md): comparing DB rows touched by write requests, UID
//! canonicalisation, XML/CSV-aware normalisation.

pub mod corpus;
pub mod normalize;

use std::time::Duration;

use anyhow::{Context, Result};
use base64::Engine;
use bytes::Bytes;
use http::{HeaderMap, HeaderValue, Method, Request, StatusCode, Uri};
use http_body_util::{BodyExt, Full};
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;

pub use corpus::Case;
pub use normalize::Rules;

#[derive(Debug, Clone)]
pub struct Captured {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Bytes,
}

pub struct Target {
    pub base: Uri,
    client: Client<HttpConnector, Full<Bytes>>,
    auth: Option<HeaderValue>,
    timeout: Duration,
    /// Sent as `Host` on every request (so both servers see the same request).
    pub host: Option<HeaderValue>,
}

impl Target {
    pub fn new(base: Uri, basic_auth: Option<(&str, &str)>, timeout: Duration) -> Result<Self> {
        let auth = basic_auth
            .map(|(u, p)| {
                let token = base64::engine::general_purpose::STANDARD.encode(format!("{u}:{p}"));
                HeaderValue::from_str(&format!("Basic {token}"))
            })
            .transpose()?;
        Ok(Self {
            base,
            client: Client::builder(TokioExecutor::new()).build(HttpConnector::new()),
            auth,
            timeout,
            host: None,
        })
    }

    pub async fn send(&self, case: &Case) -> Result<Captured> {
        let base = self.base.to_string();
        let uri: Uri = format!("{}{}", base.trim_end_matches('/'), case.path)
            .parse()
            .with_context(|| format!("bad path {:?}", case.path))?;
        let method = Method::from_bytes(case.method.as_bytes())?;
        let mut req = Request::builder().method(method).uri(uri);
        let mut has_auth = false;
        for (k, v) in &case.headers {
            has_auth |= k.eq_ignore_ascii_case("authorization");
            req = req.header(k, v);
        }
        if let Some(h) = &self.host
            && !case.headers.keys().any(|k| k.eq_ignore_ascii_case("host"))
        {
            req = req.header(http::header::HOST, h.clone());
        }
        if !has_auth && let Some(a) = &self.auth {
            req = req.header(http::header::AUTHORIZATION, a.clone());
        }
        let body = Full::new(Bytes::from(case.body.clone().unwrap_or_default()));
        let resp = tokio::time::timeout(self.timeout, self.client.request(req.body(body)?))
            .await
            .context("timed out")??;
        let (parts, body) = resp.into_parts();
        let body = body.collect().await?.to_bytes();
        Ok(Captured {
            status: parts.status,
            headers: parts.headers,
            body,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Pass,
    Fail(Vec<String>),
}

/// Compares two captured responses under `rules`; returns human-readable differences.
pub fn compare(case: &Case, java: &Captured, rust: &Captured, rules: &Rules) -> Outcome {
    let mut diffs = Vec::new();
    if java.status != rust.status {
        diffs.push(format!("status: java={} rust={}", java.status, rust.status));
    }
    for h in rules.compared_headers() {
        let j = java
            .headers
            .get(h.as_str())
            .map(|v| v.to_str().unwrap_or("<binary>"));
        let r = rust
            .headers
            .get(h.as_str())
            .map(|v| v.to_str().unwrap_or("<binary>"));
        if j != r {
            diffs.push(format!("header {h}: java={j:?} rust={r:?}"));
        }
    }
    let (jb, rb) = (
        rules.normalize_body(case, &java.headers, &java.body),
        rules.normalize_body(case, &rust.headers, &rust.body),
    );
    if jb != rb {
        let diff = similar::TextDiff::from_lines(&jb, &rb)
            .unified_diff()
            .context_radius(3)
            .header("java", "rust")
            .to_string();
        diffs.push(format!("body:\n{diff}"));
    }
    if diffs.is_empty() {
        Outcome::Pass
    } else {
        Outcome::Fail(diffs)
    }
}
