//! Loads real and synthetic `dhis.conf` files and checks the values Java would see.

use std::collections::HashMap;
use std::path::PathBuf;

use dhis_config::location::LocationManager;
use dhis_config::{
    ConfigError, ConfigurationKey as K, DhisConfigurationProvider, EncryptionStatus,
};

/// `../dhis2-core` next to this repository, or `$DHIS2_CORE_DIR`.
fn dhis2_core() -> PathBuf {
    std::env::var_os("DHIS2_CORE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../dhis2-core"))
}

fn load_dir(
    dir: &std::path::Path,
    env: &[(&str, &str)],
) -> Result<DhisConfigurationProvider, ConfigError> {
    let env: HashMap<String, String> = env
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    DhisConfigurationProvider::load_from(LocationManager::with_external_dir(dir), &env)
}

fn with_conf(content: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("dhis.conf"), content).unwrap();
    dir
}

#[test]
fn e2e_dhis_conf() {
    let dir = dhis2_core().join("dhis-2/dhis-test-e2e/config/dhis2_home");
    assert!(
        dir.join("dhis.conf").is_file(),
        "{} not found; check out dhis2-core next to this repo or set DHIS2_CORE_DIR",
        dir.display()
    );
    let c = load_dir(&dir, &[]).unwrap();

    // Values set in the file.
    assert_eq!(
        c.connection_url().as_deref(),
        Some("jdbc:postgresql://db/dhis")
    );
    assert_eq!(
        c.get_property(K::CONNECTION_USERNAME).as_deref(),
        Some("dhis")
    );
    assert_eq!(
        c.get_property(K::CONNECTION_PASSWORD).as_deref(),
        Some("dhis")
    );
    assert!(c.is_enabled(K::REDIS_ENABLED));
    assert_eq!(c.get_property(K::REDIS_HOST).as_deref(), Some("redis"));
    assert_eq!(c.get_property(K::FILESTORE_PROVIDER).as_deref(), Some("s3"));
    assert_eq!(
        c.get_property(K::FILESTORE_ENDPOINT).as_deref(),
        Some("http://s3:8333")
    );
    assert!(c.is_disabled(K::AUDIT_LOGGER));
    assert!(c.is_enabled(K::TOTP_2FA_ENABLED));
    assert!(c.is_enabled(K::EMAIL_2FA_ENABLED));
    assert!(c.is_enabled(K::SWITCH_USER_FEATURE_ENABLED));
    assert_eq!(
        c.get_property(K::SWITCH_USER_ALLOW_LISTED_IPS).as_deref(),
        Some("localhost,127.0.0.1,[0:0:0:0:0:0:0:1],0:0:0:0:0:0:0:1,172.30.0.10,172.30.0.1")
    );
    assert!(c.is_enabled(K::OAUTH2_SERVER_ENABLED));
    assert_eq!(c.server_base_url().as_deref(), Some("http://web:8080/"));
    assert_eq!(
        c.get_property(K::OIDC_DHIS2_INTERNAL_CLIENT_ID).as_deref(),
        Some("dhis2-client")
    );
    assert!(c.is_enabled(K::MONITORING_SQL_CONTEXT));
    assert_eq!(
        c.get_property(K::MONITORING_SQL_CONTEXT_KEYS).as_deref(),
        Some("controller,method,requestId")
    );
    assert_eq!(
        c.get_property(K::ROUTE_REMOTE_SERVERS_ALLOWED).as_deref(),
        Some("http://web:8080")
    );

    // Overridden in init() regardless of the file.
    assert_eq!(
        c.get_property(K::CONNECTION_DIALECT).as_deref(),
        Some("org.hisp.dhis.hibernate.dialect.DhisPostgresDialect")
    );

    // Defaults for keys not in the file.
    assert_eq!(
        c.get_property(K::SYSTEM_SESSION_TIMEOUT).as_deref(),
        Some("3600")
    );
    assert_eq!(c.get_int_property(K::CONNECTION_POOL_MAX_SIZE), Ok(80));
    assert_eq!(
        c.get_property(K::CONNECTION_POOL_TIMEOUT).as_deref(),
        Some("30000")
    );
    assert_eq!(
        c.get_property(K::MAX_FILE_UPLOAD_SIZE_BYTES).as_deref(),
        Some("10000000")
    );
    assert_eq!(
        c.get_property(K::CSP_HEADER_VALUE).as_deref(),
        Some("script-src 'none'; ")
    );
    assert_eq!(c.get_property(K::SYSTEM_MONITORING_URL), None);
    assert!(!c.is_read_only_mode());
    assert!(!c.is_cluster_enabled());
    assert!(!c.is_ldap_configured());
    assert!(!c.is_analytics_database_configured());
    assert_eq!(
        c.encryption_status(),
        EncryptionStatus::MissingEncryptionPassword
    );

    // Confidential values are blanked in the map exposed by the API.
    let map = c.configurations_as_map();
    assert_eq!(map["connection.password"], "");
    assert_eq!(map["connection.username"], "dhis");
    assert_eq!(map["system.monitoring.url"], "");
    assert_eq!(map.len(), K::values().len());
}

#[test]
fn env_substitution_and_trim() {
    let dir = with_conf(
        "connection.url = jdbc:postgresql://${DB_HOST}/${DB_NAME:-dhis}\n\
         connection.username = ${DB_USER}   \n\
         connection.password = $${NOT_SUBSTITUTED}\n",
    );
    let c = load_dir(
        dir.path(),
        &[("DB_HOST", "pg:5433"), ("DB_USER", " admin ")],
    )
    .unwrap();
    assert_eq!(
        c.connection_url().as_deref(),
        Some("jdbc:postgresql://pg:5433/dhis")
    );
    // Trimmed after substitution, so whitespace from the env var goes too.
    assert_eq!(
        c.get_property(K::CONNECTION_USERNAME).as_deref(),
        Some("admin")
    );
    assert_eq!(
        c.get_property(K::CONNECTION_PASSWORD).as_deref(),
        Some("${NOT_SUBSTITUTED}")
    );
}

#[test]
fn startup_failures_match_java() {
    let missing = tempfile::tempdir().unwrap();
    assert!(matches!(
        load_dir(missing.path(), &[]),
        Err(ConfigError::PropertiesNotLoaded(_))
    ));

    let bad_escape = with_conf("a = \\u00zz\n");
    assert!(matches!(
        load_dir(bad_escape.path(), &[]),
        Err(ConfigError::MalformedProperties(_))
    ));

    let cycle = with_conf("a = ${X}\n");
    assert!(matches!(
        load_dir(cycle.path(), &[("X", "${Y}"), ("Y", "${X}")]),
        Err(ConfigError::Substitution(_))
    ));
}

#[test]
fn alias_lookup_keeps_java_bug() {
    // Java checks aliases with Properties.contains(), i.e. against *values*. Setting the alias
    // key does nothing...
    let dir = with_conf("audit.inmemory_queue.enabled = on\n");
    let c = load_dir(dir.path(), &[]).unwrap();
    assert!(c.is_disabled(K::AUDIT_USE_IN_MEMORY_QUEUE_ENABLED));
    assert!(!c.has_property(K::AUDIT_USE_IN_MEMORY_QUEUE_ENABLED));

    // ...but any property whose value equals the alias name triggers it, and then the alias
    // key's value (null here) is returned instead of the real key's value or default.
    let dir =
        with_conf("audit.in_memory-queue.enabled = on\nsome.key = audit.inmemory_queue.enabled\n");
    let c = load_dir(dir.path(), &[]).unwrap();
    assert_eq!(c.get_property(K::AUDIT_USE_IN_MEMORY_QUEUE_ENABLED), None);
    assert!(c.has_property(K::AUDIT_USE_IN_MEMORY_QUEUE_ENABLED));
}

#[test]
fn int_property_parsing_matches_java() {
    let dir = with_conf("connection.pool.max_size = 1.5\nconnection.pool.timeout = abc\n");
    let c = load_dir(dir.path(), &[]).unwrap();
    assert!(c.get_int_property(K::CONNECTION_POOL_MAX_SIZE).is_err());
    assert_eq!(c.get_int_property(K::CONNECTION_POOL_TIMEOUT), Ok(-1));
}

#[test]
fn google_auth_client_id() {
    let dir = with_conf("");
    std::fs::write(
        dir.path().join("dhis-google-auth.json"),
        r#"{"client_id": "1234.apps"}"#,
    )
    .unwrap();
    let c = load_dir(dir.path(), &[]).unwrap();
    assert_eq!(
        c.get_property(K::GOOGLE_SERVICE_ACCOUNT_CLIENT_ID)
            .as_deref(),
        Some("1234.apps")
    );

    // Missing client_id: Properties.put(key, null) throws NPE and Java fails to start.
    std::fs::write(dir.path().join("dhis-google-auth.json"), r#"{"x": 1}"#).unwrap();
    assert!(matches!(
        load_dir(dir.path(), &[]),
        Err(ConfigError::GoogleAuthWithoutClientId)
    ));

    // Invalid JSON is only logged.
    std::fs::write(dir.path().join("dhis-google-auth.json"), "not json").unwrap();
    assert!(load_dir(dir.path(), &[]).is_ok());
}

#[test]
fn metadata_sync_servers_fall_back_to_deprecated_key() {
    let dir = with_conf("system.remote_servers_allowed = https://a.org/, https://b.org/,\n");
    let c = load_dir(dir.path(), &[]).unwrap();
    assert_eq!(
        c.metadata_sync_remote_servers_allowed(),
        vec!["https://a.org/", " https://b.org/"]
    );
    assert!(c.is_metadata_sync_remote_server_allowed(Some("https://a.org/api")));
    assert!(!c.is_metadata_sync_remote_server_allowed(Some("https://b.org/api")));
    assert!(!c.is_metadata_sync_remote_server_allowed(None));
}
