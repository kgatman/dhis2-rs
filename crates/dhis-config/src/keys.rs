//! `org.hisp.dhis.external.conf.ConfigurationKey`.
//!
//! The enum and its table are generated into `keys_generated.rs` by
//! `cargo run -p dhis-inventory -- --java-root ../dhis2-core` from the Java source, so the Rust
//! keys, defaults, confidentiality flags and aliases cannot drift from Java.

include!("keys_generated.rs");

/// One row of the generated table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyDef {
    pub name: &'static str,
    pub key: &'static str,
    pub default_value: Option<&'static str>,
    pub confidential: bool,
    pub aliases: &'static [&'static str],
}

impl ConfigurationKey {
    pub fn def(self) -> &'static KeyDef {
        &KEY_DEFS[self as usize]
    }

    /// `getKey()`.
    pub fn key(self) -> &'static str {
        self.def().key
    }

    /// `getDefaultValue()`.
    pub fn default_value(self) -> Option<&'static str> {
        self.def().default_value
    }

    /// `isConfidential()`.
    pub fn confidential(self) -> bool {
        self.def().confidential
    }

    /// `getAliases()`.
    pub fn aliases(self) -> &'static [&'static str] {
        self.def().aliases
    }

    /// `values()`.
    pub fn values() -> &'static [ConfigurationKey] {
        ALL_KEYS
    }

    /// `getByKey(String)`.
    pub fn by_key(key: &str) -> Option<ConfigurationKey> {
        ALL_KEYS.iter().copied().find(|k| k.key() == key)
    }
}
