//! Shadow routes: the client always gets Java's response; Rust's is compared in the background.

use std::sync::Arc;
use std::time::Duration;

use axum::http::header;
use axum::routing::get;
use gateway::{Config, Gateway, RouteTable, ShadowConfig, ShadowStats};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const JAVA: &str = "HTTP/1.1 200 \r\nContent-Type: application/json;charset=UTF-8\r\n\
    Transfer-Encoding: chunked\r\n\r\n\
    7\r\n{\"a\":1,\r\n\
    12\r\n\"ts\":\"2026-01-01\"}\r\n0\r\n\r\n";

async fn java_upstream() -> std::net::SocketAddr {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (mut s, _) = l.accept().await.unwrap();
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let mut seen = Vec::new();
                loop {
                    let Ok(n) = s.read(&mut buf).await else {
                        return;
                    };
                    if n == 0 {
                        return;
                    }
                    seen.extend_from_slice(&buf[..n]);
                    while let Some(i) = seen.windows(4).position(|w| w == b"\r\n\r\n") {
                        seen.drain(..i + 4);
                        if s.write_all(JAVA.as_bytes()).await.is_err() {
                            return;
                        }
                    }
                }
            });
        }
    });
    addr
}

fn json(body: &'static str) -> impl axum::response::IntoResponse {
    (
        [(header::CONTENT_TYPE, "application/json;charset=UTF-8")],
        body,
    )
}

async fn setup(rules: &str) -> (Arc<Gateway>, std::net::SocketAddr) {
    let java = java_upstream().await;
    let rust = axum::Router::new()
        .route(
            "/api/same",
            get(|| async { json(r#"{"ts":"2030-12-31","a":1}"#) }),
        )
        .route("/api/diff", get(|| async { json(r#"{"a":2,"ts":"x"}"#) }));
    let routes = RouteTable::parse(
        "[[route]]\nmethods = [\"GET\"]\npath = \"/api/**\"\nbackend = \"shadow\"\n",
    )
    .unwrap();
    let gw = Arc::new(Gateway::new(
        Config {
            java_upstream: format!("http://{java}").parse().unwrap(),
            inject_forwarded_for: false,
            shadow: ShadowConfig {
                rules: diffcheck::Rules::parse(rules).unwrap(),
                ..ShadowConfig::default()
            },
            delegate_auth: false,
            identity: None,
        },
        routes,
        rust,
    ));
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(gw.clone().serve(l));
    (gw, addr)
}

async fn get_body(addr: std::net::SocketAddr, path: &str) -> String {
    let mut s = TcpStream::connect(addr).await.unwrap();
    s.write_all(format!("GET {path} HTTP/1.1\r\nHost: h\r\nConnection: close\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut out = Vec::new();
    s.read_to_end(&mut out).await.unwrap();
    String::from_utf8_lossy(&out).into_owned()
}

async fn wait_for(gw: &Gateway, total: u64) -> ShadowStats {
    for _ in 0..100 {
        let s = gw.shadow_stats();
        if s.passed + s.failed + s.skipped >= total {
            return s;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    gw.shadow_stats()
}

#[tokio::test]
async fn client_gets_java_and_differences_are_counted() {
    let (gw, addr) =
        setup("[[ignore_key]]\nkey = \"ts\"\nreason = \"wall clock timestamp differs per call\"")
            .await;

    let resp = get_body(addr, "/api/same").await;
    assert!(resp.contains("{\"a\":1,"), "{resp}");
    let resp = get_body(addr, "/api/diff").await;
    assert!(
        resp.contains("{\"a\":1,"),
        "client must get Java's body: {resp}"
    );
    let resp = get_body(addr, "/api/unported").await;
    assert!(resp.contains("{\"a\":1,"), "{resp}");

    let s = wait_for(&gw, 3).await;
    // /api/same matches after ignoring ts; /api/diff differs; /api/unported is a Rust 404.
    assert_eq!(
        s,
        ShadowStats {
            passed: 1,
            failed: 2,
            skipped: 0
        }
    );
}

#[tokio::test]
async fn without_rules_volatile_fields_fail() {
    let (gw, addr) = setup("").await;
    get_body(addr, "/api/same").await;
    let s = wait_for(&gw, 1).await;
    assert_eq!(s.failed, 1);
}
