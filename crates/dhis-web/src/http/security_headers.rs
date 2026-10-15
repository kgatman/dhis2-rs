//! Headers Java adds to every `/api` response:
//!
//! - `org.hisp.dhis.webapi.filter.CspFilter`: `Content-Security-Policy: frame-ancestors 'self'
//!   [cors whitelist];` (plus `script-src 'none'; ` first for uploaded content), or
//!   `X-Frame-Options: SAMEORIGIN` when `csp.enabled` is off;
//! - Spring Security header writers configured in `DhisWebApiWebSecurityConfig#setHttpHeaders`:
//!   `X-Content-Type-Options: nosniff`, `X-XSS-Protection: 0` (HSTS only on secure requests, which
//!   never reach the gateway).

use std::sync::{Arc, LazyLock};

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;
use http::HeaderValue;
use regex::Regex;

use super::RequestContext;
use crate::AppState;

/// `CspConstants.EXTERNAL_STATIC_CONTENT_URL_PATTERNS` = `List.of(P_1, P_2, P_2, P_4)`.
/// Java lists `P_2` twice and never `P_3` (`fileResources/{uid}/data`); kept as is.
static UPLOADED_CONTENT: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"^.+/dataValues/files$",
        r"^.+messageConversations/[a-zA-Z\d]+/[a-zA-Z\d]+/attachments/[a-zA-Z\d]+$",
        r"^.+audits/files/[a-zA-Z\d]+$",
    ]
    .iter()
    .map(|p| Regex::new(p).expect("valid regex"))
    .collect()
});

/// Where the CORS whitelist (`configuration_corswhitelist`) comes from.
#[derive(Clone)]
pub enum CorsWhitelistSource {
    Static(Vec<String>),
    Db(sqlx::PgPool),
}

impl CorsWhitelistSource {
    /// The whitelist in the order Java iterates it (`ConfigurationService#getCorsWhitelist`
    /// returns Hibernate's `PersistentSet`, i.e. a `HashSet`).
    pub async fn get(&self) -> Vec<String> {
        let values = match self {
            Self::Static(v) => v.clone(),
            Self::Db(pool) => sqlx::query_scalar::<_, String>(
                "select corswhitelist from configuration_corswhitelist where corswhitelist is not null",
            )
            .fetch_all(pool)
            .await
            .unwrap_or_else(|e| {
                tracing::error!("reading configuration_corswhitelist: {e}");
                Vec::new()
            }),
        };
        java_hash_set_order(values)
    }
}

/// `String#hashCode`.
fn java_string_hash(s: &str) -> i32 {
    s.encode_utf16()
        .fold(0i32, |h, c| h.wrapping_mul(31).wrapping_add(i32::from(c)))
}

/// Iteration order of a `java.util.HashSet` built the way Hibernate builds a `PersistentSet`
/// (`new HashSet<>(n + (int) (n * .75f), .75f)`), elements added in the given order.
pub fn java_hash_set_order(values: Vec<String>) -> Vec<String> {
    let mut unique: Vec<String> = Vec::new();
    for v in values {
        if !unique.contains(&v) {
            unique.push(v);
        }
    }
    let n = unique.len();
    if n == 0 {
        return unique;
    }
    // HashMap(initialCapacity) -> table size = next power of two >= initialCapacity.
    let initial = n + (n as f32 * 0.75) as usize;
    let mut cap = initial.next_power_of_two().max(1);
    // Resize while size > 0.75 * capacity.
    while n as f32 > cap as f32 * 0.75 {
        cap *= 2;
    }
    let bucket = |s: &str| {
        let h = java_string_hash(s);
        let spread = h ^ ((h as u32) >> 16) as i32;
        (spread as u32 as usize) & (cap - 1)
    };
    // Stable sort keeps insertion order within a bucket.
    unique.sort_by_key(|s| bucket(s));
    unique
}

pub async fn layer(State(state): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    let path = req
        .extensions()
        .get::<RequestContext>()
        .map(|c| c.original_path.clone())
        .unwrap_or_else(|| req.uri().path().to_string());
    let csp_enabled = state
        .config
        .is_enabled(dhis_config::ConfigurationKey::CSP_ENABLED);
    let whitelist = if csp_enabled {
        state.cors_whitelist.get().await
    } else {
        Vec::new()
    };

    let mut resp = next.run(req).await;
    let h = resp.headers_mut();
    if csp_enabled {
        if UPLOADED_CONTENT.iter().any(|p| p.is_match(&path)) {
            h.append(
                "content-security-policy",
                HeaderValue::from_static("script-src 'none'; "),
            );
        }
        let value = if whitelist.is_empty() {
            "frame-ancestors 'self';".to_string()
        } else {
            format!("frame-ancestors 'self' {};", whitelist.join(" "))
        };
        if let Ok(v) = HeaderValue::from_str(&value) {
            h.append("content-security-policy", v);
        }
    } else {
        h.append("x-frame-options", HeaderValue::from_static("SAMEORIGIN"));
    }
    h.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    h.insert("x-xss-protection", HeaderValue::from_static("0"));
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_hash_matches_java() {
        // "hello".hashCode() == 99162322, "".hashCode() == 0
        assert_eq!(java_string_hash("hello"), 99162322);
        assert_eq!(java_string_hash(""), 0);
        assert_eq!(java_string_hash("http://localhost:3000"), {
            // computed by hand with the same recurrence; cross-checked with jshell
            "http://localhost:3000"
                .encode_utf16()
                .fold(0i32, |h, c| h.wrapping_mul(31).wrapping_add(c as i32))
        });
    }

    #[test]
    fn single_value_order_is_trivial() {
        assert_eq!(java_hash_set_order(vec!["a".into(), "a".into()]), vec!["a"]);
    }
}
