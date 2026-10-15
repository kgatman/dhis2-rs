//! HTTP handlers for endpoints ported from `dhis-web-api`.
//!
//! Java counterpart: `dhis-web-api` (`org.hisp.dhis.webapi.controller.*`). Each ported controller
//! gets one module here, named after the Java class (e.g. `schema_controller` for
//! `org.hisp.dhis.webapi.controller.SchemaController`), and is only reachable once the gateway's
//! route table sends its paths to the `rust` backend.
//!
//! Phase 0/1: no endpoints are ported yet, so the router is empty.

use axum::Router;

/// The router mounted by the gateway for routes whose backend is `rust`.
pub fn router() -> Router {
    Router::new()
}
