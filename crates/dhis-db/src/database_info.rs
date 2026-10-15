//! `org.hisp.dhis.system.database.HibernateDatabaseInfoProvider`.

use std::sync::LazyLock;

use dhis_config::{ConfigurationKey as K, DhisConfigurationProvider};
use regex::Regex;
use sqlx::PgPool;

/// `POSTGRES_VERSION_REGEX`.
static VERSION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^([a-zA-Z_-]+ \d+\.+\d+)?[ ,].*$").expect("valid regex"));

/// `DatabaseInfo` without `time` (that one is per request).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatabaseInfo {
    pub name: Option<String>,
    pub user: Option<String>,
    pub url: Option<String>,
    pub database_version: Option<String>,
    /// Always true in Java "for backwards compatibility".
    pub spatial_support: bool,
}

/// `HibernateDatabaseInfoProvider#init` (startup checks for extensions are Java's job while it
/// runs alongside).
pub async fn load(pool: &PgPool, config: &DhisConfigurationProvider) -> DatabaseInfo {
    let internal: Option<(String, String, String)> =
        sqlx::query_as("select version(), current_catalog::text, current_user::text")
            .fetch_one(pool)
            .await
            .map_err(|e| tracing::error!("An error occurred when retrieving database info. {e}"))
            .ok();
    // Java falls back to ("", "", "") on error.
    let (version, name, user) = internal.unwrap_or_default();
    let database_version = match VERSION.captures(&version) {
        // find() succeeded: group(1), which may be null.
        Some(c) => c.get(1).map(|m| m.as_str().to_string()),
        None => Some(version),
    };
    let config_user = config.get_property(K::CONNECTION_USERNAME);
    DatabaseInfo {
        name: Some(name),
        user: if user.is_empty() {
            config_user
        } else {
            Some(user)
        },
        url: config.get_property(K::CONNECTION_URL),
        database_version,
        spatial_support: true,
    }
}

/// `select now()` as epoch millis (`java.util.Date` precision).
pub async fn now_millis(pool: &PgPool) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("select floor(extract(epoch from now()) * 1000)::bigint")
        .fetch_one(pool)
        .await
}

#[cfg(test)]
mod tests {
    use super::VERSION;

    #[test]
    fn version_regex_like_java() {
        let v = "PostgreSQL 16.15 on aarch64-unknown-linux-musl, compiled by gcc";
        assert_eq!(&VERSION.captures(v).unwrap()[1], "PostgreSQL 16.15");
        // Optional group absent: find() succeeds with group(1) == null.
        assert!(VERSION.captures(" x").unwrap().get(1).is_none());
    }
}
