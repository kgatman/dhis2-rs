use std::collections::HashMap;
use std::time::Duration;

use dhis_config::DhisConfigurationProvider;
use dhis_config::location::LocationManager;
use dhis_db::{DbError, PoolSettings, connect_options};

fn config(conf: &str) -> DhisConfigurationProvider {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("dhis.conf"), conf).unwrap();
    DhisConfigurationProvider::load_from(
        LocationManager::with_external_dir(dir.path()),
        &HashMap::new(),
    )
    .unwrap()
}

#[test]
fn hikari_defaults() {
    let c = config("connection.url = jdbc:postgresql://db/dhis\nconnection.username = dhis\n");
    let s = PoolSettings::from_config(&c).unwrap();
    assert_eq!(s.max_size, 80);
    assert_eq!(s.min_idle, 10);
    assert_eq!(s.connection_timeout, Duration::from_secs(30));
    assert_eq!(s.validation_timeout, Duration::from_secs(5));
    assert_eq!(s.keep_alive, Duration::from_secs(120));
    assert_eq!(s.idle_timeout, Duration::from_secs(600));
    assert_eq!(s.max_lifetime, Duration::from_secs(1800));
    assert_eq!(s.test_query, None);

    let o = connect_options(&c).unwrap();
    assert_eq!(o.get_host(), "db");
    assert_eq!(o.get_port(), 5432);
    assert_eq!(o.get_database(), Some("dhis"));
    assert_eq!(o.get_username(), "dhis");
}

#[test]
fn bad_numbers_fail_like_java() {
    let c = config("connection.url = jdbc:postgresql://db/dhis\nconnection.pool.max_size = lots\n");
    assert!(matches!(
        PoolSettings::from_config(&c),
        Err(DbError::BadNumber { .. })
    ));
}

#[test]
fn missing_url() {
    let c = config("");
    assert!(matches!(connect_options(&c), Err(DbError::MissingUrl)));
}
