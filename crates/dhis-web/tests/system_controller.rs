//! SystemController against the shapes measured on the Java server at the pinned ref.

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Body;
use dhis_auth::CurrentUser;
use dhis_config::DhisConfigurationProvider;
use dhis_config::location::LocationManager;
use dhis_web::http::{JavaFallback, RequestContext};
use dhis_web::{AppState, CorsWhitelistSource};
use http::{Method, Request, Response, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

fn app() -> axum::Router {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("dhis.conf"), "").unwrap();
    let config = DhisConfigurationProvider::load_from(
        LocationManager::with_external_dir(dir.path()),
        &HashMap::new(),
    )
    .unwrap();
    dhis_web::router(Arc::new(AppState {
        config: Arc::new(config),
        cors_whitelist: CorsWhitelistSource::Static(vec![]),
    }))
}

fn admin() -> CurrentUser {
    CurrentUser {
        id: "M5zQapPyTZI".into(),
        username: "admin".into(),
        authorities: vec!["ALL".into()],
    }
}

async fn call(
    method: Method,
    uri: &str,
    accept: Option<&str>,
    ext: Option<&str>,
) -> (Response<Body>, String) {
    let mut b = Request::builder().method(method).uri(uri);
    if let Some(a) = accept {
        b = b.header("accept", a);
    }
    let mut req = b.body(Body::empty()).unwrap();
    req.extensions_mut().insert(admin());
    req.extensions_mut().insert(RequestContext {
        original_path: uri.split('?').next().unwrap().to_string(),
        extension: ext.map(str::to_string),
    });
    let resp = app().oneshot(req).await.unwrap();
    let (parts, body) = resp.into_parts();
    let bytes = body.collect().await.unwrap().to_bytes();
    (
        Response::from_parts(parts, Body::empty()),
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

fn h<'a>(r: &'a Response<Body>, name: &str) -> Option<&'a str> {
    r.headers().get(name).and_then(|v| v.to_str().ok())
}

#[tokio::test]
async fn uid_json() {
    let (r, body) = call(Method::GET, "/api/system/uid", None, None).await;
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(
        h(&r, "content-type"),
        Some("application/json;charset=UTF-8")
    );
    assert_eq!(
        h(&r, "cache-control"),
        Some("no-cache, no-store, max-age=0, must-revalidate")
    );
    assert_eq!(
        h(&r, "content-security-policy"),
        Some("frame-ancestors 'self';")
    );
    assert_eq!(h(&r, "x-content-type-options"), Some("nosniff"));
    assert_eq!(h(&r, "x-xss-protection"), Some("0"));
    assert_eq!(h(&r, "etag"), None, "no-store responses get no ETag");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let codes = v["codes"].as_array().unwrap();
    assert_eq!(codes.len(), 1);
    assert!(dhis_core_model::uid::is_valid_uid(
        codes[0].as_str().unwrap()
    ));
    assert_eq!(body.len(), 25);
}

#[tokio::test]
async fn limits() {
    for (q, n) in [
        ("limit=3", 3),
        ("limit=-5", 0),
        ("limit=20000", 10_000),
        ("limit=0x10", 16),
        ("limit=", 1),
        ("limit=%203", 3),
    ] {
        let (_, body) = call(Method::GET, &format!("/api/system/id?{q}"), None, None).await;
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["codes"].as_array().unwrap().len(), n, "{q}");
    }
}

#[tokio::test]
async fn csv_has_no_content_type_but_an_etag() {
    let (r, body) = call(
        Method::GET,
        "/api/system/uid?limit=2",
        Some("application/csv"),
        None,
    )
    .await;
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(h(&r, "content-type"), None);
    assert_eq!(h(&r, "cache-control"), None);
    assert!(
        body.starts_with("uid\n") && body.lines().count() == 3,
        "{body:?}"
    );
    assert_eq!(
        h(&r, "etag"),
        Some(dhis_web::http::etag::etag_for(body.as_bytes()).as_str())
    );
    // First listed wins when equally specific.
    let (r, _) = call(
        Method::GET,
        "/api/system/uid",
        Some("application/csv, application/json"),
        None,
    )
    .await;
    assert_eq!(h(&r, "content-type"), None);
}

#[tokio::test]
async fn head_has_no_etag() {
    let (r, body) = call(
        Method::HEAD,
        "/api/system/uid",
        Some("application/csv"),
        None,
    )
    .await;
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(h(&r, "etag"), None);
    assert_eq!(body, "");
}

#[tokio::test]
async fn bad_limit_is_a_web_message() {
    let (r, body) = call(Method::GET, "/api/system/uid?limit=1.5", None, None).await;
    assert_eq!(r.status(), StatusCode::BAD_REQUEST);
    assert_eq!(h(&r, "content-type"), Some("application/json"));
    assert_eq!(h(&r, "cache-control"), Some("no-cache, private"));
    assert_eq!(
        body,
        r#"{"httpStatus":"Bad Request","httpStatusCode":400,"status":"ERROR","message":"Value '1.5' is not valid for parameter limit. For input string: \"1.5\""}"#
    );
}

#[tokio::test]
async fn unsupported_representations_fall_back_to_java() {
    for (accept, ext) in [
        (Some("text/csv"), None),
        (Some("application/xml"), None),
        (None, Some("csv")),
    ] {
        // limit=abc must not win over the 406 Java gives for these.
        let mut req = Request::builder().uri("/api/system/uid?limit=abc");
        if let Some(a) = accept {
            req = req.header("accept", a);
        }
        let mut req = req.body(Body::empty()).unwrap();
        req.extensions_mut().insert(admin());
        req.extensions_mut().insert(RequestContext {
            original_path: "/api/system/uid".into(),
            extension: ext.map(str::to_string),
        });
        let r = app().oneshot(req).await.unwrap();
        assert!(
            r.extensions().get::<JavaFallback>().is_some(),
            "{accept:?} {ext:?}"
        );
    }
    let (r, _) = call(
        Method::GET,
        "/api/system/uuid",
        Some("application/xml"),
        None,
    )
    .await;
    assert!(r.extensions().get::<JavaFallback>().is_some());
}

#[tokio::test]
async fn uuid_json() {
    let (r, body) = call(Method::GET, "/api/system/uuid?limit=2", None, None).await;
    assert_eq!(
        h(&r, "content-type"),
        Some("application/json;charset=UTF-8")
    );
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["codes"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn unauthenticated_requests_fall_back() {
    let req = Request::builder()
        .uri("/api/system/uid")
        .body(Body::empty())
        .unwrap();
    let r = app().oneshot(req).await.unwrap();
    assert!(r.extensions().get::<JavaFallback>().is_some());
}
