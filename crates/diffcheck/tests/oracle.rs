use std::time::Duration;

use axum::Router;
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::get;
use diffcheck::{Case, Outcome, Rules, Target, compare};

async fn serve(router: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    format!("http://{addr}")
}

fn json(body: &'static str) -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "application/json;charset=UTF-8")],
        body,
    )
}

fn case(path: &str) -> Case {
    Case {
        name: path.into(),
        method: "GET".into(),
        path: path.into(),
        headers: Default::default(),
        body: None,
        tags: vec![],
    }
}

#[tokio::test]
async fn passes_after_normalisation_and_fails_on_real_differences() {
    let java = serve(
        Router::new()
            .route(
                "/a",
                get(|| async { json(r#"{"id":"x","lastUpdated":"2026-01-01"}"#) }),
            )
            .route("/b", get(|| async { json(r#"{"id":"x"}"#) })),
    )
    .await;
    let rust = serve(
        Router::new()
            .route(
                "/a",
                get(|| async { json(r#"{"lastUpdated":"2026-02-02","id":"x"}"#) }),
            )
            .route(
                "/b",
                get(|| async { (StatusCode::CREATED, json(r#"{"id":"y"}"#)) }),
            ),
    )
    .await;

    let t = Duration::from_secs(5);
    let j = Target::new(java.parse().unwrap(), Some(("admin", "district")), t).unwrap();
    let r = Target::new(rust.parse().unwrap(), Some(("admin", "district")), t).unwrap();
    let rules =
        Rules::parse("[[ignore_key]]\nkey = \"lastUpdated\"\nreason = \"timestamp set on write\"")
            .unwrap();

    let a = case("/a");
    let out = compare(
        &a,
        &j.send(&a).await.unwrap(),
        &r.send(&a).await.unwrap(),
        &rules,
    );
    assert_eq!(out, Outcome::Pass);

    let b = case("/b");
    let Outcome::Fail(diffs) = compare(
        &b,
        &j.send(&b).await.unwrap(),
        &r.send(&b).await.unwrap(),
        &rules,
    ) else {
        panic!("expected failure");
    };
    assert!(diffs[0].starts_with("status: java=200 OK rust=201 Created"));
    assert!(
        diffs
            .iter()
            .any(|d| d.contains("-  \"id\": \"x\"") && d.contains("+  \"id\": \"y\""))
    );
}
