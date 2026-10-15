//! Loads `DHIS2_HOME/dhis.conf` exactly like the Java server.
//!
//! Ported from `org.hisp.dhis.external.conf.DefaultDhisConfigurationProvider`,
//! `org.hisp.dhis.external.conf.DhisConfigurationProvider` and
//! `org.hisp.dhis.external.conf.ConfigurationKey` (module `dhis-support-external`).
//!
//! The key table in [`keys`] is generated from `ConfigurationKey.java` by `tools/inventory`; do
//! not edit it by hand.

pub mod java;
pub mod keys;
pub mod location;
pub mod properties;
pub mod substitutor;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use thiserror::Error;
use tracing::{info, warn};

pub use keys::ConfigurationKey;
use location::LocationManager;

pub const CONF_FILENAME: &str = "dhis.conf";
pub const GOOGLE_AUTH_FILENAME: &str = "dhis-google-auth.json";

/// Each variant corresponds to an exception that makes the Java server fail at startup.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// `IllegalStateException("Properties could not be loaded", cause)`.
    #[error("Properties could not be loaded: {0}")]
    PropertiesNotLoaded(String),
    /// `IllegalArgumentException` from `Properties.load`.
    #[error("{0}")]
    MalformedProperties(#[from] properties::PropertiesError),
    /// `IllegalStateException` from `StringSubstitutor`.
    #[error("{0}")]
    Substitution(#[from] substitutor::SubstitutionError),
    /// `NullPointerException` from `Properties.put(key, null)` when `dhis-google-auth.json` has no
    /// `client_id`. Java does not catch it, so startup fails.
    #[error("dhis-google-auth.json has no client_id (NullPointerException in Java)")]
    GoogleAuthWithoutClientId,
}

/// A value stored in Java's `Properties` (a `Hashtable<Object, Object>`).
///
/// Everything from `dhis.conf` is a string, but `client_id` from `dhis-google-auth.json` is put
/// in as whatever JSON type it has. `Properties#getProperty` ignores non-string values.
#[derive(Debug, Clone, PartialEq)]
pub enum PropertyValue {
    String(String),
    Other(serde_json::Value),
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Properties(HashMap<String, PropertyValue>);

impl Properties {
    /// `Properties#getProperty(String)`.
    pub fn get_property(&self, key: &str) -> Option<&str> {
        match self.0.get(key) {
            Some(PropertyValue::String(s)) => Some(s),
            _ => None,
        }
    }

    pub fn set_property(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.0
            .insert(key.into(), PropertyValue::String(value.into()));
    }

    /// `Hashtable#contains(Object)`: true if any **value** equals the argument.
    pub fn contains_value(&self, value: &str) -> bool {
        self.0
            .values()
            .any(|v| matches!(v, PropertyValue::String(s) if s == value))
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &PropertyValue)> {
        self.0.iter()
    }
}

/// Where to look for DHIS2_HOME and which environment to substitute from.
#[derive(Debug, Clone, Default)]
pub struct LoadOptions {
    /// Stand-in for the JVM system property `-Ddhis2.home`.
    pub dhis2_home: Option<String>,
    /// Environment used for `DHIS2_HOME` lookup and `${VAR}` substitution.
    pub env: HashMap<String, String>,
}

impl LoadOptions {
    /// Uses the process environment, like `System.getenv()`.
    pub fn from_process(dhis2_home: Option<String>) -> Self {
        let env = std::env::vars_os()
            .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
            .collect();
        Self { dhis2_home, env }
    }
}

/// `DefaultDhisConfigurationProvider`.
#[derive(Debug, Clone)]
pub struct DhisConfigurationProvider {
    properties: Properties,
    location: LocationManager,
}

/// `EncryptionStatus`, minus `MISSING_JCE_POLICY` which cannot happen on Java 17+.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncryptionStatus {
    Ok,
    MissingEncryptionPassword,
    EncryptionPasswordTooShort,
}

impl DhisConfigurationProvider {
    /// `DefaultLocationManager#init` + `DefaultDhisConfigurationProvider#init`.
    pub fn load(opts: &LoadOptions) -> Result<Self, ConfigError> {
        let location = LocationManager::init(
            opts.dhis2_home.as_deref(),
            opts.env.get(location::DEFAULT_ENV_VAR).map(String::as_str),
        );
        Self::load_from(location, &opts.env)
    }

    /// Loads from an already-resolved location (e.g. a fixed directory in tests).
    pub fn load_from(
        location: LocationManager,
        env: &HashMap<String, String>,
    ) -> Result<Self, ConfigError> {
        let mut properties = load_dhis_conf(&location, env)?;
        properties.set_property(
            "connection.dialect",
            "org.hisp.dhis.hibernate.dialect.DhisPostgresDialect",
        );
        load_google_client_id(&location, &mut properties)?;
        Ok(Self {
            properties,
            location,
        })
    }

    pub fn location(&self) -> &LocationManager {
        &self.location
    }

    pub fn home(&self) -> Option<&Path> {
        self.location.external_directory().ok()
    }

    /// `getProperties()`.
    pub fn properties(&self) -> &Properties {
        &self.properties
    }

    /// `getProperty(key)`: the value, or the key's default (which may be null).
    pub fn get_property(&self, key: ConfigurationKey) -> Option<String> {
        self.get_property_or_default(key, key.default_value())
    }

    /// `getPropertyOrDefault(key, defaultValue)`.
    ///
    /// Java checks aliases with `Properties.contains(alias)`, which tests **values**, not keys.
    /// So an alias only takes effect if some property's value equals the alias name, and then the
    /// alias key's value (often absent, i.e. null) is returned. We keep that bug; see
    /// docs/deviations.md ("bugs kept").
    pub fn get_property_or_default(
        &self,
        key: ConfigurationKey,
        default_value: Option<&str>,
    ) -> Option<String> {
        for alias in key.aliases() {
            if self.properties.contains_value(alias) {
                return self.properties.get_property(alias).map(str::to_string);
            }
        }
        self.properties
            .get_property(key.key())
            .or(default_value)
            .map(str::to_string)
    }

    /// `hasProperty(key)`, including the same alias quirk as [`Self::get_property_or_default`].
    pub fn has_property(&self, key: ConfigurationKey) -> bool {
        let mut value = self.properties.get_property(key.key());
        for alias in key.aliases() {
            if self.properties.contains_value(alias) {
                value = Some(alias);
            }
        }
        value.is_some_and(|v| !v.is_empty())
    }

    /// `isEnabled(key)`.
    pub fn is_enabled(&self, key: ConfigurationKey) -> bool {
        java::is_on(self.get_property(key).as_deref())
    }

    /// `isDisabled(key)`.
    pub fn is_disabled(&self, key: ConfigurationKey) -> bool {
        !self.is_enabled(key)
    }

    /// `getIntProperty(key)`; `Err` where Java throws `NumberFormatException`.
    pub fn get_int_property(
        &self,
        key: ConfigurationKey,
    ) -> Result<i32, java::NumberFormatException> {
        java::to_int(self.get_property(key).as_deref())
    }

    /// `getConnectionUrl()`.
    pub fn connection_url(&self) -> Option<String> {
        self.get_property(ConfigurationKey::CONNECTION_URL)
    }

    /// `isReadOnlyMode()`.
    pub fn is_read_only_mode(&self) -> bool {
        self.is_enabled(ConfigurationKey::SYSTEM_READ_ONLY_MODE)
    }

    /// `isClusterEnabled()`.
    pub fn is_cluster_enabled(&self) -> bool {
        let not_blank = |k| {
            self.get_property(k)
                .is_some_and(|v| !v.chars().all(java::is_whitespace))
        };
        not_blank(ConfigurationKey::CLUSTER_MEMBERS)
            && not_blank(ConfigurationKey::CLUSTER_HOSTNAME)
    }

    /// `getServerBaseUrl()`: raw property, `trimToNull`, no default.
    pub fn server_base_url(&self) -> Option<String> {
        self.properties
            .get_property(ConfigurationKey::SERVER_BASE_URL.key())
            .map(java::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    }

    /// `getMetaDataSyncRemoteServersAllowed()`. Java still reads the deprecated key as fallback.
    #[allow(deprecated)]
    pub fn metadata_sync_remote_servers_allowed(&self) -> Vec<String> {
        let raw = |k: ConfigurationKey| {
            java::strip(self.properties.get_property(k.key()).unwrap_or("")).to_string()
        };
        let mut servers = raw(ConfigurationKey::META_DATA_SYNC_SERVERS_ALLOWED);
        if servers.is_empty() {
            servers = raw(ConfigurationKey::REMOTE_SERVERS_ALLOWED);
        }
        servers
            .split(',')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect()
    }

    /// `isMetaDataSyncRemoteServerAllowed(url)`.
    pub fn is_metadata_sync_remote_server_allowed(&self, url: Option<&str>) -> bool {
        match url {
            Some(u) if !u.chars().all(java::is_whitespace) => {
                let allowed = self.metadata_sync_remote_servers_allowed();
                !allowed.is_empty() && allowed.iter().any(|a| u.starts_with(a.as_str()))
            }
            _ => false,
        }
    }

    /// `isLdapConfigured()`.
    pub fn is_ldap_configured(&self) -> bool {
        let url = self.get_property(ConfigurationKey::LDAP_URL);
        let manager_dn = self.get_property(ConfigurationKey::LDAP_MANAGER_DN);
        !(url.as_deref() == ConfigurationKey::LDAP_URL.default_value()
            || url.is_none()
            || manager_dn.is_none())
    }

    /// `isAnalyticsDatabaseConfigured()`.
    pub fn is_analytics_database_configured(&self) -> bool {
        self.get_property(ConfigurationKey::ANALYTICS_CONNECTION_URL)
            .is_some_and(|v| !v.chars().all(java::is_whitespace))
    }

    /// `getEncryptionStatus()` (Java 17+ always has unlimited JCE policy).
    pub fn encryption_status(&self) -> EncryptionStatus {
        let password = self
            .get_property(ConfigurationKey::ENCRYPTION_PASSWORD)
            .unwrap_or_default();
        // Java's String#length counts UTF-16 units.
        let len = password.encode_utf16().count();
        if len == 0 {
            EncryptionStatus::MissingEncryptionPassword
        } else if len < 24 {
            EncryptionStatus::EncryptionPasswordTooShort
        } else {
            EncryptionStatus::Ok
        }
    }

    /// `getConfigurationsAsMap()`: confidential keys are blanked.
    pub fn configurations_as_map(&self) -> BTreeMap<&'static str, String> {
        ConfigurationKey::values()
            .iter()
            .map(|&k| {
                let v = if k.confidential() {
                    String::new()
                } else {
                    self.get_property_or_default(k, Some(k.default_value().unwrap_or("")))
                        .unwrap_or_default()
                };
                (k.key(), v)
            })
            .collect()
    }
}

/// `loadDhisConf()` + `substituteEnvironmentVariables()`.
fn load_dhis_conf(
    location: &LocationManager,
    env: &HashMap<String, String>,
) -> Result<Properties, ConfigError> {
    let not_loaded = |e: &dyn std::fmt::Display| {
        tracing::debug!("Could not load {CONF_FILENAME}: {e}");
        ConfigError::PropertiesNotLoaded(e.to_string())
    };
    let path = location
        .file_for_reading(CONF_FILENAME, &[])
        .map_err(|e| not_loaded(&e))?;
    let bytes = std::fs::read(&path).map_err(|e| not_loaded(&e))?;

    let mut props = Properties::default();
    for (k, v) in properties::parse(&bytes)? {
        props.set_property(k, v);
    }
    // Java iterates the Hashtable in hash order; only the error message for the first failing
    // entry could depend on it, and any failure aborts startup either way.
    for value in props.0.values_mut() {
        if let PropertyValue::String(s) = value {
            *s = java::trim(&substitutor::replace(s, env)?).to_string();
        }
    }
    Ok(props)
}

/// The `dhis-google-auth.json` part of `init()`: copies `client_id` into the properties.
///
/// Building the Google credential itself is not ported yet (see PORT_STATUS.md, phase 8).
fn load_google_client_id(
    location: &LocationManager,
    properties: &mut Properties,
) -> Result<(), ConfigError> {
    let Ok(path) = location.file_for_reading(GOOGLE_AUTH_FILENAME, &[]) else {
        info!("Could not find {GOOGLE_AUTH_FILENAME}");
        return Ok(());
    };
    let json: serde_json::Map<String, serde_json::Value> = match std::fs::read(&path)
        .map_err(|e| e.to_string())
        .and_then(|b| serde_json::from_slice(&b).map_err(|e| e.to_string()))
    {
        Ok(j) => j,
        Err(e) => {
            warn!("Could not load credential from {GOOGLE_AUTH_FILENAME}: {e}");
            return Ok(());
        }
    };
    let key = ConfigurationKey::GOOGLE_SERVICE_ACCOUNT_CLIENT_ID
        .key()
        .to_string();
    match json.get("client_id") {
        None | Some(serde_json::Value::Null) => Err(ConfigError::GoogleAuthWithoutClientId),
        Some(serde_json::Value::String(s)) => {
            properties.set_property(key, s.clone());
            Ok(())
        }
        Some(other) => {
            properties
                .0
                .insert(key, PropertyValue::Other(other.clone()));
            Ok(())
        }
    }
}

/// Convenience for binaries: the directory a provider was loaded from.
pub fn conf_path(provider: &DhisConfigurationProvider) -> Option<PathBuf> {
    provider.home().map(|h| h.join(CONF_FILENAME))
}
