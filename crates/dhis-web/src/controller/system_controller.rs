//! Ported from `org.hisp.dhis.webapi.controller.SystemController` (partial).
//!
//! | Java handler | Mapping | Status |
//! |---|---|---|
//! | `getUid` | `GET /api/system/{uid,id}` | ported |
//! | `getUidCsv` | `GET /api/system/{uid,id}`, produces `application/csv` | ported |
//! | `getUuid` | `GET /api/system/uuid`, produces JSON/XML | JSON ported, XML falls back |
//!
//! Behaviour measured against Java at the pinned ref (see tests).

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::Request;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use http::{HeaderValue, header};
use serde::Serialize;

use crate::http::negotiation::{Representation, choose};
use crate::http::{RequestContext, fallback};
use crate::params::Params;
use crate::{AppState, Authenticated};

/// `SystemController.generateCodeList`: `Math.min(limit, 10000)` items (none if negative).
const MAX_CODES: i32 = 10_000;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/system/uid", get(get_uid))
        .route("/api/system/id", get(get_uid))
        .route("/api/system/uuid", get(get_uuid))
}

/// `org.hisp.dhis.common.CodeList`.
#[derive(Serialize)]
struct CodeList {
    codes: Vec<String>,
}

/// `ContextUtils.setNoStore(response)`.
const NO_STORE: &str = "no-cache, no-store, max-age=0, must-revalidate";

/// JSON as written by Spring's Jackson converter for a `@ResponseBody`.
fn json_body(value: &impl Serialize) -> Response {
    let mut r = Response::new(Body::from(serde_json::to_vec(value).unwrap_or_default()));
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json;charset=UTF-8"),
    );
    r
}

fn codes(limit: i32, generate: fn() -> String) -> Vec<String> {
    (0..limit.min(MAX_CODES)).map(|_| generate()).collect()
}

async fn get_uid(_user: Authenticated, params: Params, req: Request) -> Response {
    let ctx = req.extensions().get::<RequestContext>();
    let rep = choose(
        req.headers(),
        ctx,
        &[Representation::Json, Representation::ApplicationCsv],
    );
    // Spring selects the handler (produces vs Accept) before binding parameters.
    let Some(rep) = rep else {
        return fallback("content negotiation");
    };
    let limit = match params.integer_or("limit", 1) {
        Ok(l) => l,
        Err(m) => return m.into_response(),
    };
    match rep {
        Representation::Json => {
            let mut r = json_body(&CodeList {
                codes: codes(limit, dhis_core_model::uid::generate_uid),
            });
            r.headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static(NO_STORE));
            r
        }
        // getUidCsv writes straight to the output stream: no Content-Type, no Cache-Control.
        Representation::ApplicationCsv => {
            let mut body = String::from("uid\n");
            for c in codes(limit, dhis_core_model::uid::generate_uid) {
                body.push_str(&c);
                body.push('\n');
            }
            Response::new(Body::from(body))
        }
    }
}

async fn get_uuid(_user: Authenticated, params: Params, req: Request) -> Response {
    let ctx = req.extensions().get::<RequestContext>();
    // produces = {application/json, application/xml}; XML is not ported yet.
    match choose(req.headers(), ctx, &[Representation::Json]) {
        Some(Representation::Json) => {}
        _ => return fallback("content negotiation"),
    }
    let limit = match params.integer_or("limit", 1) {
        Ok(l) => l,
        Err(m) => return m.into_response(),
    };
    let mut r = json_body(&CodeList {
        codes: codes(limit, dhis_core_model::uid::random_uuid),
    });
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static(NO_STORE));
    r
}
