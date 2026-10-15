//! PostgreSQL access.
//!
//! Java counterparts: `org.hisp.dhis.datasource.DatabasePoolUtils` (Hikari pool, module
//! `dhis-support-hibernate`) and the PostgreSQL JDBC driver's URL parsing. Explicit SQL replacing
//! the Hibernate mappings lands here per slice (see `docs/inventory/tables.csv`).
//!
//! Note: during the strangler phase Rust and Java share one database, so the two pools together
//! must fit in Postgres' `max_connections`. [`PoolSettings::max_size_override`] exists for that.

pub mod database_info;
pub mod jdbc;
pub mod settings;

use std::time::Duration;

use dhis_config::{ConfigurationKey as K, DhisConfigurationProvider};
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};
use thiserror::Error;

pub use jdbc::JdbcUrl;

#[derive(Debug, Error)]
pub enum DbError {
    #[error("connection.url is not set")]
    MissingUrl,
    #[error(transparent)]
    Jdbc(#[from] jdbc::JdbcUrlError),
    /// Java throws `NumberFormatException` while building the pool and fails to start.
    #[error("{key}: not a number: {value:?}")]
    BadNumber { key: &'static str, value: String },
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
}

/// The Hikari settings DHIS2 derives from `dhis.conf` (`createHikariDbPool`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolSettings {
    pub connection_timeout: Duration,
    pub validation_timeout: Duration,
    pub max_size: u32,
    pub min_idle: u32,
    pub keep_alive: Duration,
    pub idle_timeout: Duration,
    pub max_lifetime: Duration,
    /// `connection.pool.preferred.test.query`; sqlx always pings instead (see deviations).
    pub test_query: Option<String>,
    /// Rust-side cap so Java + Rust stay under `max_connections` while both run.
    pub max_size_override: Option<u32>,
}

fn number<T: std::str::FromStr>(c: &DhisConfigurationProvider, key: K) -> Result<T, DbError> {
    let value = c.get_property(key).unwrap_or_default();
    // Java's Long.parseLong / Integer.parseInt accept a leading '+' but no whitespace.
    value.parse().map_err(|_| DbError::BadNumber {
        key: key.key(),
        value,
    })
}

impl PoolSettings {
    pub fn from_config(c: &DhisConfigurationProvider) -> Result<Self, DbError> {
        Ok(Self {
            connection_timeout: Duration::from_millis(number(c, K::CONNECTION_POOL_TIMEOUT)?),
            validation_timeout: Duration::from_millis(number(
                c,
                K::CONNECTION_POOL_VALIDATION_TIMEOUT,
            )?),
            max_size: number(c, K::CONNECTION_POOL_MAX_SIZE)?,
            min_idle: number(c, K::CONNECTION_POOL_MIN_IDLE)?,
            keep_alive: Duration::from_secs(number(c, K::CONNECTION_POOL_KEEP_ALIVE_TIME_SECONDS)?),
            idle_timeout: Duration::from_secs(number(c, K::CONNECTION_POOL_MAX_IDLE_TIME)?),
            max_lifetime: Duration::from_secs(number(c, K::CONNECTION_POOL_MAX_LIFETIME_SECONDS)?),
            test_query: c
                .get_property(K::CONNECTION_POOL_TEST_QUERY)
                .filter(|q| !q.trim().is_empty()),
            max_size_override: None,
        })
    }

    pub fn pool_options(&self) -> PgPoolOptions {
        let max = self.max_size_override.unwrap_or(self.max_size);
        PgPoolOptions::new()
            .max_connections(max)
            .min_connections(self.min_idle.min(max))
            .acquire_timeout(self.connection_timeout)
            .idle_timeout(Some(self.idle_timeout))
            .max_lifetime(Some(self.max_lifetime))
            .test_before_acquire(true)
    }
}

/// Connection options for the main database (`connection.url/username/password`).
pub fn connect_options(c: &DhisConfigurationProvider) -> Result<PgConnectOptions, DbError> {
    let url = c
        .connection_url()
        .filter(|u| !u.is_empty())
        .ok_or(DbError::MissingUrl)?;
    let jdbc = JdbcUrl::parse(&url)?;
    Ok(jdbc.connect_options(
        c.get_property(K::CONNECTION_USERNAME).as_deref(),
        c.get_property(K::CONNECTION_PASSWORD).as_deref(),
    ))
}

/// Creates the pool lazily (no connection is opened until first use).
pub fn pool(
    c: &DhisConfigurationProvider,
    max_size_override: Option<u32>,
) -> Result<PgPool, DbError> {
    let mut settings = PoolSettings::from_config(c)?;
    settings.max_size_override = max_size_override;
    Ok(settings
        .pool_options()
        .connect_lazy_with(connect_options(c)?))
}
