//! Byte-level tests: a raw TCP "Java" upstream and a raw TCP client around a real gateway, so we
//! can assert exactly what goes over the wire on both sides.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::routing::get;
use gateway::{Config, Gateway, RouteTable};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// A fake upstream that records each raw request and answers with a fixed raw response.
struct Upstream {
    addr: SocketAddr,
    requests: Arc<Mutex<Vec<String>>>,
}

async fn upstream(response: &'static str) -> Upstream {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let rec = requests.clone();
    tokio::spawn(async move {
        loop {
            let (mut s, _) = listener.accept().await.unwrap();
            let rec = rec.clone();
            tokio::spawn(async move {
                loop {
                    let Some(req) = read_http_message(&mut s).await else {
                        return;
                    };
                    rec.lock().unwrap().push(req);
                    if s.write_all(response.as_bytes()).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    Upstream { addr, requests }
}

/// Reads one HTTP/1.1 message (head + Content-Length or chunked body) as text.
async fn read_http_message(s: &mut TcpStream) -> Option<String> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let head_end = loop {
        if let Some(i) = find(&buf, b"\r\n\r\n") {
            break i + 4;
        }
        let n = s.read(&mut tmp).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_lowercase();
    let content_length = head
        .lines()
        .find_map(|l| l.strip_prefix("content-length:"))
        .and_then(|v| v.trim().parse::<usize>().ok());
    let chunked = head.contains("transfer-encoding: chunked");
    loop {
        let body = &buf[head_end..];
        let done = match content_length {
            Some(n) => body.len() >= n,
            None if chunked => find(body, b"0\r\n\r\n").is_some(),
            None => true,
        };
        if done {
            break;
        }
        let n = s.read(&mut tmp).await.ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    Some(String::from_utf8_lossy(&buf).to_string())
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

async fn gateway(java: SocketAddr, routes: RouteTable, inject_xff: bool) -> SocketAddr {
    let rust = axum::Router::new()
        .route("/api/ping", get(|| async { "pong" }))
        .route(
            "/api/partial",
            get(|| async { dhis_web::http::fallback("not ported") }),
        );
    let gw = Arc::new(Gateway::new(
        Config {
            java_upstream: format!("http://{java}").parse().unwrap(),
            inject_forwarded_for: inject_xff,
            shadow: Default::default(),
            delegate_auth: false,
        },
        routes,
        rust,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(gw.serve(listener));
    addr
}

/// Sends a request with `Connection: close` and returns everything until EOF (interim 1xx
/// responses included).
async fn raw_exchange_to_eof(gw: SocketAddr, req: &str) -> String {
    let mut s = TcpStream::connect(gw).await.unwrap();
    s.write_all(req.as_bytes()).await.unwrap();
    let mut out = Vec::new();
    s.read_to_end(&mut out).await.unwrap();
    String::from_utf8_lossy(&out).to_string()
}

async fn raw_request(gw: SocketAddr, req: &str) -> String {
    let mut s = TcpStream::connect(gw).await.unwrap();
    s.write_all(req.as_bytes()).await.unwrap();
    read_http_message(&mut s).await.unwrap()
}

const JAVA_RESPONSE: &str = "HTTP/1.1 201 \r\n\
    Content-Type: application/json;charset=UTF-8\r\n\
    Set-Cookie: JSESSIONID=abc; Path=/; HttpOnly\r\n\
    Set-Cookie: other=1\r\n\
    X-Weird-CASE: v\r\n\
    Keep-Alive: timeout=60\r\n\
    Content-Length: 11\r\n\r\n\
    {\"id\":\"x\"}\n";

#[tokio::test]
async fn request_passes_through_unchanged_except_hop_by_hop() {
    let java = upstream(JAVA_RESPONSE).await;
    let gw = gateway(java.addr, RouteTable::all_java(), true).await;

    let resp = raw_request(
        gw,
        "GET /api/41/me?fields=id,name&paging=false HTTP/1.1\r\n\
         Host: web:8080\r\n\
         X-Custom-Header: a\r\n\
         Cookie: JSESSIONID=abc\r\n\
         Connection: keep-alive, X-Drop-Me\r\n\
         X-Drop-Me: 1\r\n\
         Proxy-Authorization: secret\r\n\
         Accept: application/json\r\n\r\n",
    )
    .await;

    let reqs = java.requests.lock().unwrap().clone();
    assert_eq!(reqs.len(), 1);
    let r = &reqs[0];
    assert!(
        r.starts_with("GET /api/41/me?fields=id,name&paging=false HTTP/1.1\r\n"),
        "{r}"
    );
    assert!(r.contains("\r\nHost: web:8080\r\n"), "{r}");
    assert!(r.contains("\r\nX-Custom-Header: a\r\n"), "{r}");
    assert!(r.contains("\r\nCookie: JSESSIONID=abc\r\n"), "{r}");
    assert!(r.contains("\r\nAccept: application/json\r\n"), "{r}");
    assert!(
        r.contains("\r\nx-forwarded-for: 127.0.0.1\r\n")
            || r.contains("\r\nX-Forwarded-For: 127.0.0.1\r\n"),
        "{r}"
    );
    assert!(!r.to_lowercase().contains("x-drop-me"), "{r}");
    assert!(!r.to_lowercase().contains("proxy-authorization"), "{r}");

    // Tomcat omits the reason phrase; hyper keeps it that way (ReasonPhrase extension).
    assert!(resp.starts_with("HTTP/1.1 201 \r\n"), "{resp:?}");
    assert!(resp.contains("\r\nX-Weird-CASE: v\r\n"), "{resp}");
    assert!(
        resp.contains("\r\nSet-Cookie: JSESSIONID=abc; Path=/; HttpOnly\r\n"),
        "{resp}"
    );
    assert!(resp.contains("\r\nSet-Cookie: other=1\r\n"), "{resp}");
    assert!(
        resp.contains("\r\nContent-Type: application/json;charset=UTF-8\r\n"),
        "{resp}"
    );
    assert!(
        !resp.to_lowercase().contains("keep-alive: timeout"),
        "{resp}"
    );
    assert!(resp.ends_with("{\"id\":\"x\"}\n"), "{resp}");
}

#[tokio::test]
async fn bodies_stream_through() {
    let java = upstream("HTTP/1.1 200 \r\nContent-Length: 0\r\n\r\n").await;
    let gw = gateway(java.addr, RouteTable::all_java(), true).await;

    let body = "x".repeat(200_000);
    let resp = raw_exchange_to_eof(
        gw,
        &format!(
            "POST /api/metadata HTTP/1.1\r\nHost: h\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nExpect: 100-continue\r\nConnection: close\r\n\r\n{body}",
            body.len()
        ),
    )
    .await;
    // hyper answers the client's Expect itself; the final response follows.
    assert!(
        resp.starts_with("HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200"),
        "{resp}"
    );

    let reqs = java.requests.lock().unwrap().clone();
    let r = &reqs[0];
    assert!(r.starts_with("POST /api/metadata HTTP/1.1\r\n"));
    assert!(r.ends_with(&body));
    assert!(!r.to_lowercase().contains("expect:"));

    // Chunked request bodies arrive intact too.
    let resp = raw_request(
        gw,
        "PUT /api/x HTTP/1.1\r\nHost: h\r\nTransfer-Encoding: chunked\r\n\r\n\
         5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n",
    )
    .await;
    assert!(resp.starts_with("HTTP/1.1 200"));
    let reqs = java.requests.lock().unwrap().clone();
    let r = &reqs[1];
    assert!(r.contains("hello") && r.contains(" world"), "{r}");
}

#[tokio::test]
async fn forwarded_for_is_kept_or_not_injected() {
    let java = upstream("HTTP/1.1 204 \r\n\r\n").await;
    let gw = gateway(java.addr, RouteTable::all_java(), true).await;
    raw_request(
        gw,
        "GET / HTTP/1.1\r\nHost: h\r\nX-Forwarded-For: 10.0.0.1, 10.0.0.2\r\n\r\n",
    )
    .await;
    let r = java.requests.lock().unwrap()[0].clone();
    assert!(r.contains("X-Forwarded-For: 10.0.0.1, 10.0.0.2\r\n"), "{r}");
    assert_eq!(r.to_lowercase().matches("x-forwarded-for").count(), 1);

    let java = upstream("HTTP/1.1 204 \r\n\r\n").await;
    let gw = gateway(java.addr, RouteTable::all_java(), false).await;
    raw_request(gw, "GET / HTTP/1.1\r\nHost: h\r\n\r\n").await;
    let r = java.requests.lock().unwrap()[0].clone();
    assert!(!r.to_lowercase().contains("x-forwarded-for"), "{r}");
}

#[tokio::test]
async fn upstream_down_is_502() {
    let dead = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = dead.local_addr().unwrap();
    drop(dead);
    let gw = gateway(addr, RouteTable::all_java(), true).await;
    let resp = raw_request(gw, "GET /api/me HTTP/1.1\r\nHost: h\r\n\r\n").await;
    assert!(resp.starts_with("HTTP/1.1 502"), "{resp}");
}

#[tokio::test]
async fn rust_routes_bypass_java() {
    let java = upstream("HTTP/1.1 200 \r\nContent-Length: 4\r\n\r\njava").await;
    let routes = RouteTable::parse(
        "[[route]]\nmethods = [\"GET\"]\npath = \"/api/ping\"\nbackend = \"rust\"\n",
    )
    .unwrap();
    let gw = gateway(java.addr, routes, true).await;

    let resp = raw_request(gw, "GET /api/ping HTTP/1.1\r\nHost: h\r\n\r\n").await;
    assert!(resp.ends_with("pong"), "{resp}");
    let resp = raw_request(
        gw,
        "POST /api/ping HTTP/1.1\r\nHost: h\r\nContent-Length: 0\r\n\r\n",
    )
    .await;
    assert!(resp.ends_with("java"), "{resp}");
    assert_eq!(java.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn response_uses_client_http_version() {
    let java = upstream("HTTP/1.0 200 OK\r\nContent-Length: 2\r\n\r\nok").await;
    let gw = gateway(java.addr, RouteTable::all_java(), true).await;
    let resp = raw_request(gw, "GET / HTTP/1.1\r\nHost: h\r\n\r\n").await;
    assert!(resp.starts_with("HTTP/1.1 200"), "{resp:?}");
}

#[tokio::test]
async fn fallback_replays_the_original_request_to_java() {
    let java = upstream("HTTP/1.1 200 \r\nContent-Length: 4\r\n\r\njava").await;
    let routes = RouteTable::parse(
        "[[route]]\nmethods = [\"GET\"]\npath = \"/api/partial\"\nbackend = \"rust\"\n",
    )
    .unwrap();
    let gw = gateway(java.addr, routes, false).await;
    // Versioned path + extension: Rust sees the normalised path, Java the original one.
    let resp = raw_request(
        gw,
        "GET /api/41/partial.json?x=1 HTTP/1.1\r\nHost: h\r\nX-Custom: a\r\n\r\n",
    )
    .await;
    assert!(resp.ends_with("java"), "{resp}");
    let r = java.requests.lock().unwrap()[0].clone();
    assert!(
        r.starts_with("GET /api/41/partial.json?x=1 HTTP/1.1\r\n"),
        "{r}"
    );
    assert!(r.contains("X-Custom: a\r\n"), "{r}");
}
