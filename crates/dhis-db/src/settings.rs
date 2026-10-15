//! Reading stored settings (`SystemSettingsStore`; table `systemsetting`).

use dhis_core_model::settings::Settings;
use sqlx::PgPool;

/// All stored system settings, as `SystemSettingsProvider#getCurrentSettings` sees them (Java
/// caches this and invalidates on write; reading per request is always current).
pub async fn system_settings(pool: &PgPool) -> Result<Settings, sqlx::Error> {
    let rows: Vec<(String, Option<String>)> =
        sqlx::query_as("select name, value from systemsetting")
            .fetch_all(pool)
            .await?;
    Ok(Settings::of(rows))
}
