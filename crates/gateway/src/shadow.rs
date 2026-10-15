//! Shadow mode: compare Rust against Java on live read traffic without affecting clients.
//!
//! The client gets Java's response, streamed as usual. A [`Tee`] copies its body (up to a limit)
//! as it passes. The same request runs against the Rust router in a background task, and when
//! both are complete they are compared with `diffcheck`'s rules. Results go to the log and,
//! optionally, to a JSON-lines file.

use std::io::Write;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};

use bytes::Bytes;
use diffcheck::{Captured, Case, Outcome, Rules};
use http::{HeaderMap, Method, Response, StatusCode};
use http_body_util::BodyExt;
use hyper::body::{Body as HttpBody, Frame, SizeHint};
use tokio::sync::oneshot;
use tracing::{info, warn};

use crate::{Body, BoxError};

#[derive(Debug, Clone)]
pub struct ShadowConfig {
    /// Normalisation rules (same format as `diffcheck --rules`).
    pub rules: Rules,
    /// Append one JSON object per compared request.
    pub log_path: Option<PathBuf>,
    /// Bodies larger than this (either side) are not compared.
    pub body_limit: usize,
}

impl Default for ShadowConfig {
    fn default() -> Self {
        Self {
            rules: Rules::default(),
            log_path: None,
            body_limit: 8 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ShadowStats {
    pub passed: u64,
    pub failed: u64,
    pub skipped: u64,
}

pub(crate) struct Shadow {
    config: ShadowConfig,
    passed: AtomicU64,
    failed: AtomicU64,
    skipped: AtomicU64,
    log: Mutex<Option<std::fs::File>>,
}

impl Shadow {
    pub(crate) fn new(config: ShadowConfig) -> Self {
        let log = config.log_path.as_ref().and_then(|p| {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(p)
                .map_err(|e| warn!("cannot open shadow log {}: {e}", p.display()))
                .ok()
        });
        Self {
            config,
            passed: AtomicU64::new(0),
            failed: AtomicU64::new(0),
            skipped: AtomicU64::new(0),
            log: Mutex::new(log),
        }
    }

    pub(crate) fn body_limit(&self) -> usize {
        self.config.body_limit
    }

    pub(crate) fn stats(&self) -> ShadowStats {
        ShadowStats {
            passed: self.passed.load(Ordering::Relaxed),
            failed: self.failed.load(Ordering::Relaxed),
            skipped: self.skipped.load(Ordering::Relaxed),
        }
    }

    pub(crate) async fn compare(
        &self,
        method: Method,
        path_and_query: String,
        java_status: StatusCode,
        java_headers: HeaderMap,
        java_body: oneshot::Receiver<Option<Bytes>>,
        rust: Response<axum::body::Body>,
    ) {
        let (rust_parts, rust_body) = rust.into_parts();
        let rust_body = http_body_util::Limited::new(rust_body, self.config.body_limit)
            .collect()
            .await;
        let java_body = java_body.await.ok().flatten();

        let (java_body, rust_body) = match (java_body, rust_body) {
            (Some(j), Ok(r)) => (j, r.to_bytes()),
            _ => {
                self.skipped.fetch_add(1, Ordering::Relaxed);
                self.record(
                    &method,
                    &path_and_query,
                    "skipped",
                    &["body too large, or a stream was interrupted".into()],
                );
                return;
            }
        };

        let case = Case {
            name: "shadow".into(),
            method: method.to_string(),
            path: path_and_query.clone(),
            headers: Default::default(),
            body: None,
            tags: vec![],
        };
        let java = Captured {
            status: java_status,
            headers: java_headers,
            body: java_body,
        };
        let rust = Captured {
            status: rust_parts.status,
            headers: rust_parts.headers,
            body: rust_body,
        };
        match diffcheck::compare(&case, &java, &rust, &self.config.rules) {
            Outcome::Pass => {
                self.passed.fetch_add(1, Ordering::Relaxed);
                info!(target: "shadow", %method, path = %path_and_query, "match");
                self.record(&method, &path_and_query, "pass", &[]);
            }
            Outcome::Fail(diffs) => {
                self.failed.fetch_add(1, Ordering::Relaxed);
                warn!(target: "shadow", %method, path = %path_and_query, "MISMATCH\n{}", diffs.join("\n"));
                self.record(&method, &path_and_query, "fail", &diffs);
            }
        }
    }

    fn record(&self, method: &Method, path: &str, outcome: &str, diffs: &[String]) {
        let mut guard = self.log.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(f) = guard.as_mut() {
            let line = serde_json::json!({
                "method": method.as_str(),
                "path": path,
                "outcome": outcome,
                "diffs": diffs,
            });
            let _ = writeln!(f, "{line}");
        }
    }
}

/// A body that passes frames through unchanged and keeps a copy of the data (up to `limit`).
/// On completion it sends the copy, or `None` if it overflowed, failed or was dropped early.
pub(crate) struct Tee {
    inner: Body,
    buf: Vec<u8>,
    limit: usize,
    overflow: bool,
    failed: bool,
    ended: bool,
    tx: Option<oneshot::Sender<Option<Bytes>>>,
}

impl Tee {
    pub(crate) fn new(inner: Body, limit: usize) -> (Self, oneshot::Receiver<Option<Bytes>>) {
        let (tx, rx) = oneshot::channel();
        (
            Self {
                inner,
                buf: Vec::new(),
                limit,
                overflow: false,
                failed: false,
                ended: false,
                tx: Some(tx),
            },
            rx,
        )
    }

    fn finish(&mut self) {
        if let Some(tx) = self.tx.take() {
            // `ended` is set when the inner body returned None; `is_end_stream` covers empty
            // bodies that are never polled (not every body reports it after the end).
            let done = self.ended || self.inner.is_end_stream();
            let complete = !self.overflow && !self.failed && done;
            let _ = tx.send(complete.then(|| Bytes::from(std::mem::take(&mut self.buf))));
        }
    }
}

impl HttpBody for Tee {
    type Data = Bytes;
    type Error = BoxError;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, BoxError>>> {
        let this = &mut *self;
        match Pin::new(&mut this.inner).poll_frame(cx) {
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(d) = frame.data_ref()
                    && !this.overflow
                {
                    if this.buf.len() + d.len() > this.limit {
                        this.overflow = true;
                        this.buf = Vec::new();
                    } else {
                        this.buf.extend_from_slice(d);
                    }
                }
                Poll::Ready(Some(Ok(frame)))
            }
            Poll::Ready(Some(Err(e))) => {
                this.failed = true;
                this.finish();
                Poll::Ready(Some(Err(e)))
            }
            Poll::Ready(None) => {
                this.ended = true;
                this.finish();
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

impl Drop for Tee {
    fn drop(&mut self) {
        // Covers empty bodies that hyper never polls, and clients that went away mid-stream.
        self.finish();
    }
}
