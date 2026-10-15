//! Ported from `org.hisp.dhis.webapi.controller.user.MeController` (partial).
//!
//! | Java handler | Mapping | Status |
//! |---|---|---|
//! | `getAuthorities` | `GET /api/me/{authorization,authorities}` | ported |
//! | `hasAuthority` | `GET /api/me/{authorization,authorities}/{authority}` | ported |
//! | `getSettings` | `GET /api/me/settings` | ported |
//! | `getSetting` | `GET /api/me/settings/{key}` | ported |
//! | `getCurrentUser` and the rest | | Java |
//!
//! Both the authorities and the user settings are per-session state in Java. They come from the
//! delegated authentication call (`/api/me?fields=...,authorities,settings`), which Java answers
//! from the same session state (see [`dhis_auth::CurrentUser::settings`]).
//!
//! Behaviour measured against Java at the pinned ref (corpus/phase2/me.jsonl).

use std::sync::Arc;

use axum::Router;
use axum::extract::rejection::PathRejection;
use axum::extract::{Path, Request};
use axum::response::Response;
use axum::routing::get;
use dhis_auth::JsonMembers;
use dhis_core_model::java::hash::{Construction, hash_set_order};
use dhis_core_model::java::json;
use dhis_core_model::locale::Locale;
use dhis_core_model::settings::{is_confidential, user_definitions};
use http::{HeaderValue, header};

use crate::http::negotiation::{Representation, choose};
use crate::http::{RequestContext, fallback};
use crate::params::Params;
use crate::{AppState, Authenticated};

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/me/authorities", get(get_authorities))
        .route("/api/me/authorization", get(get_authorities))
        .route("/api/me/authorities/{authority}", get(has_authority))
        .route("/api/me/authorization/{authority}", get(has_authority))
        .route("/api/me/settings", get(get_settings))
        .route("/api/me/settings/{key}", get(get_setting))
}

/// Every handler here `produces = application/json`.
fn json_only(req: &Request) -> bool {
    choose(
        req.headers(),
        req.extensions().get::<RequestContext>(),
        &[Representation::Json],
    ) == Some(Representation::Json)
}

fn json_response(body: String, cache_control: Option<&'static str>) -> Response {
    let mut r = crate::http::json_response(body);
    if let Some(cc) = cache_control {
        r.headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static(cc));
    }
    r
}

/// `getAuthorities`: `UserDetails#getAllAuthorities()`, a `new HashSet<>(Set.copyOf(...))`, so
/// the order is the hash table's. Authorities sharing a bucket keep their insertion order, which
/// comes from an immutable set whose order Java randomises per JVM start; the order Java lists
/// them in `/api/me` is used for that (it matches in the cases measured, but is not guaranteed).
async fn get_authorities(Authenticated(user): Authenticated, req: Request) -> Response {
    if !json_only(&req) {
        return fallback("content negotiation");
    }
    let Some(order) = hash_set_order(
        user.authorities.iter().map(String::as_str),
        Construction::Presized,
    ) else {
        return fallback("treeified hash bin");
    };
    json_response(json::to_string(&order), Some("no-store"))
}

/// `hasAuthority`: `UserDetails#isAuthorized(authority)`.
async fn has_authority(
    Authenticated(user): Authenticated,
    authority: Result<Path<String>, PathRejection>,
    req: Request,
) -> Response {
    if !json_only(&req) {
        return fallback("content negotiation");
    }
    let Ok(Path(authority)) = authority else {
        return fallback("path variable");
    };
    let body = if user.is_authorized(&authority) {
        "true"
    } else {
        "false"
    };
    json_response(body.into(), Some("no-store"))
}

/// The current user's settings for this request: the session snapshot, with the per-request
/// `keyDbLocale` override of `UserContextInterceptor` (`translate=true&locale=...`).
struct CurrentSettings<'a> {
    snapshot: &'a JsonMembers,
    db_locale_override: Option<String>,
}

impl<'a> CurrentSettings<'a> {
    fn of(snapshot: &'a JsonMembers, params: &Params) -> Self {
        let db_locale_override = match (params.first("translate"), params.first("locale")) {
            (Some("true"), Some(locale)) if !locale.is_empty() => {
                // asLocale: the raw value parsed, the default locale if it does not parse.
                let default = user_definitions()
                    .get("keyDbLocale")
                    .and_then(|d| d.default.as_str())
                    .unwrap_or("en");
                let rendered =
                    Locale::parse(locale).map_or_else(|_| default.to_string(), |l| l.to_string());
                Some(json::string_literal(&rendered))
            }
            _ => None,
        };
        Self {
            snapshot,
            db_locale_override,
        }
    }

    /// `asJson(key)` of a non-confidential key, as JSON text; `None` when that depends on raw
    /// values the snapshot does not show.
    fn value(&self, key: &str) -> Option<String> {
        if key == "keyDbLocale"
            && let Some(v) = &self.db_locale_override
        {
            return Some(v.clone());
        }
        if let Some(v) = self.snapshot.get(key) {
            return Some(v.to_string());
        }
        // Every stored non-confidential key is in the snapshot, so this key has no value and no
        // default: `""`. Without the `key` prefix, Java (`indexOf`) looks up `key<Name>` and would
        // render its raw value, which the snapshot does not show; that is only known to be absent
        // when `key<Name>` is not in the snapshot and not confidential. The empty key throws in
        // Java unless the user has no settings stored at all, which is not known either.
        if key.starts_with("key") {
            return Some("\"\"".into());
        }
        let first = key.chars().next()?;
        if !first.is_ascii() {
            // Character.toUpperCase on a UTF-16 unit is not modelled.
            return None;
        }
        let prefixed = format!("key{}{}", first.to_ascii_uppercase(), &key[1..]);
        (self.snapshot.get(&prefixed).is_none() && !is_confidential(&prefixed))
            .then(|| "\"\"".into())
    }
}

/// Java writes setting keys into the JSON verbatim (measured: `a\b` comes out raw, `"` and
/// control characters make it fail with a 500), so only keys that need no escaping are handled.
fn verbatim_key(key: &str) -> bool {
    !key.chars().any(|c| c == '"' || c == '\\' || c < ' ')
}

/// `getSettings`: `toJson(false)`, or `toJson(false, key)` for a non-empty `key` set.
async fn get_settings(
    Authenticated(user): Authenticated,
    params: Params,
    req: Request,
) -> Response {
    if !json_only(&req) {
        return fallback("content negotiation");
    }
    let Some(snapshot) = &user.settings else {
        return fallback("no user settings snapshot");
    };
    let settings = CurrentSettings::of(snapshot, &params);
    let mut members: Vec<(String, String)> = Vec::new();
    match params.string_set("key").filter(|keys| !keys.is_empty()) {
        // All keys, in the order Java produced them for this session (the override replaces a
        // key that is always present, so the order is unchanged).
        None => {
            for key in snapshot.keys() {
                let Some(value) = settings.value(key) else {
                    return fallback("user setting value");
                };
                members.push((key.to_string(), value));
            }
        }
        // `new HashSet<>(filter)`, minus confidential keys.
        Some(keys) => {
            let Some(order) =
                hash_set_order(keys.iter().map(String::as_str), Construction::Presized)
            else {
                return fallback("treeified hash bin");
            };
            for key in order.into_iter().filter(|k| !is_confidential(k)) {
                let Some(value) = settings.value(key) else {
                    return fallback("user setting value");
                };
                members.push((key.to_string(), value));
            }
        }
    }
    let mut body = String::from("{");
    for (i, (key, value)) in members.iter().enumerate() {
        if !verbatim_key(key) {
            return fallback("setting key needs escaping");
        }
        if i > 0 {
            body.push(',');
        }
        body.push('"');
        body.push_str(key);
        body.push_str("\":");
        body.push_str(value);
    }
    body.push('}');
    json_response(body, Some("no-cache, private"))
}

/// `getSetting`: `toJson(false, Set.of(key)).get(key)`; a confidential key is absent from that
/// object, so the result is `null`.
async fn get_setting(
    Authenticated(user): Authenticated,
    key: Result<Path<String>, PathRejection>,
    params: Params,
    req: Request,
) -> Response {
    if !json_only(&req) {
        return fallback("content negotiation");
    }
    let Ok(Path(key)) = key else {
        return fallback("path variable");
    };
    let Some(snapshot) = &user.settings else {
        return fallback("no user settings snapshot");
    };
    if !verbatim_key(&key) {
        return fallback("setting key needs escaping");
    }
    let body = if is_confidential(&key) {
        "null".to_string()
    } else {
        match CurrentSettings::of(snapshot, &params).value(&key) {
            Some(v) => v,
            None => return fallback("user setting value"),
        }
    };
    json_response(body, None)
}
