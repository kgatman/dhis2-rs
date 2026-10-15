//! HTTP handlers for endpoints ported from `dhis-web-api`.
//!
//! Java counterpart: `dhis-web-api` (`org.hisp.dhis.webapi.controller.*`). Each ported controller
//! gets one module under [`controller`], named after the Java class, and is only reachable once
//! the gateway's route table sends its paths to the `rust` (or `shadow`) backend.
//!
//! Requests arrive already authenticated by the gateway ([`dhis_auth::CurrentUser`] extension)
//! and with the path normalised the way Java normalises it ([`http::RequestContext`]).

pub mod controller;
pub mod http;
pub mod java;
pub mod params;

use std::sync::Arc;

use axum::Router;
use axum::extract::FromRequestParts;
use axum::response::Response;
use dhis_auth::CurrentUser;
use dhis_config::DhisConfigurationProvider;

pub use crate::http::security_headers::CorsWhitelistSource;

/// Shared state of all handlers.
pub struct AppState {
    pub config: Arc<DhisConfigurationProvider>,
    pub cors_whitelist: CorsWhitelistSource,
    /// The shared database; handlers that need it fall back to Java when absent (tests).
    pub db: Option<sqlx::PgPool>,
    /// The Java server, for values mirrored from it during the transition (ADR 0005).
    pub java: Option<Arc<dhis_auth::delegated::JavaAuthenticator>>,
    pub system_info: controller::system_controller::SystemInfoCache,
}

impl AppState {
    /// State without database or Java access (handlers needing them fall back).
    pub fn minimal(config: Arc<DhisConfigurationProvider>) -> Self {
        Self {
            config,
            cors_whitelist: CorsWhitelistSource::Static(vec![]),
            db: None,
            java: None,
            system_info: Default::default(),
        }
    }
}

/// The router mounted by the gateway for routes whose backend is `rust` or `shadow`.
pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .merge(controller::system_controller::routes())
        .merge(controller::me_controller::routes())
        .merge(controller::schema_controller::routes())
        .merge(controller::metadata_controller::routes())
        .layer(axum::middleware::from_fn(crate::http::rfd::layer))
        .layer(axum::middleware::from_fn(crate::http::etag::layer))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::http::security_headers::layer,
        ))
        .with_state(state)
}

/// What delegated authentication must fetch for a request to this (normalised) path: the
/// settings endpoints need the user settings snapshot, the Gist lists both (authorities for the
/// access checks, settings for the translation locale), everything else the authorities.
pub fn auth_probe(normalized_path: &str) -> dhis_auth::delegated::Probe {
    use dhis_auth::delegated::Probe;
    if normalized_path == "/api/me/settings" || normalized_path.starts_with("/api/me/settings/") {
        Probe::Settings
    } else if controller::metadata_controller::needs_settings(normalized_path) {
        Probe::AuthoritiesAndSettings
    } else {
        Probe::Authorities
    }
}

/// Whether handlers for this (normalised) path need the session's identity (user id and group
/// ids, `CurrentUser::identity`) for sharing checks.
pub fn needs_identity(normalized_path: &str) -> bool {
    controller::metadata_controller::needs_settings(normalized_path)
}

/// Extracts the authenticated user; without one (the gateway did not authenticate), the request
/// goes back to Java.
pub struct Authenticated(pub CurrentUser);

impl<S: Send + Sync> FromRequestParts<S> for Authenticated {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut ::http::request::Parts,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<CurrentUser>()
            .cloned()
            .map(Authenticated)
            .ok_or_else(|| crate::http::fallback("no authenticated user"))
    }
}
