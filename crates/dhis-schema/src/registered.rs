//! Typed view of the registered schemas captured from the pinned Java build (ADR 0007).
//!
//! The JSON keeps Java's own values, Hibernate-derived ones included (`persisted`, `required`,
//! relation flags, ...). Property order is the order of the captured run's `HashMap`; Java's own
//! order can differ after a JVM restart (deviation M4), which only affects member order where
//! Java iterates `Schema#getProperties()`.

use std::collections::HashMap;
use std::sync::LazyLock;

use serde::Deserialize;

use crate::{Property, REGISTERED_SCHEMAS_JSON};

/// `org.hisp.dhis.schema.Schema` (the members Rust uses).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisteredSchema {
    pub klass: String,
    pub singular: String,
    pub plural: String,
    pub name: String,
    #[serde(default)]
    pub collection_name: Option<String>,
    /// e.g. `/constants`; `None` for types without their own endpoint.
    #[serde(default)]
    pub relative_api_endpoint: Option<String>,
    #[serde(default)]
    pub api_endpoint: Option<String>,
    #[serde(default)]
    pub shareable: bool,
    #[serde(default)]
    pub data_shareable: bool,
    #[serde(default)]
    pub implicit_private_authority: bool,
    #[serde(default)]
    pub identifiable_object: bool,
    #[serde(default)]
    pub nameable_object: bool,
    #[serde(default)]
    pub persisted: bool,
    #[serde(default)]
    pub metadata: bool,
    #[serde(default)]
    pub authorities: Vec<SchemaAuthority>,
    /// In captured (`HashMap`) order.
    pub properties: Vec<Property>,
    #[serde(skip)]
    by_key: HashMap<String, usize>,
}

/// `org.hisp.dhis.security.Authority` of a schema.
#[derive(Debug, Clone, Deserialize)]
pub struct SchemaAuthority {
    #[serde(rename = "type")]
    pub kind: String,
    pub authorities: Vec<String>,
}

impl RegisteredSchema {
    /// `Schema#getAuthorityByType(type)`: all authorities of that type, first occurrence first.
    pub fn authorities_by_type(&self, kind: &str) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        for a in self.authorities.iter().filter(|a| a.kind == kind) {
            for x in &a.authorities {
                if !out.contains(&x.as_str()) {
                    out.push(x);
                }
            }
        }
        out
    }

    /// `Schema#getProperty(name)`: properties are keyed by `Property#key()`.
    pub fn property(&self, key: &str) -> Option<&Property> {
        self.by_key.get(key).map(|&i| &self.properties[i])
    }

    /// `Schema#hasApiEndpoint()`.
    pub fn has_api_endpoint(&self) -> bool {
        self.relative_api_endpoint.is_some() || self.api_endpoint.is_some()
    }

    /// The simple class name (`Class#getSimpleName()` for top-level classes).
    pub fn simple_name(&self) -> &str {
        self.klass.rsplit('.').next().unwrap_or(&self.klass)
    }
}

struct Registry {
    schemas: Vec<RegisteredSchema>,
    by_klass: HashMap<String, usize>,
    by_singular: HashMap<String, usize>,
}

static REGISTRY: LazyLock<Registry> = LazyLock::new(|| {
    let mut schemas: Vec<RegisteredSchema> = serde_json::from_str(REGISTERED_SCHEMAS_JSON)
        .unwrap_or_else(|e| panic!("bad registered schemas data: {e}"));
    for s in &mut schemas {
        s.by_key = s
            .properties
            .iter()
            .enumerate()
            .map(|(i, p)| (p.key().to_string(), i))
            .collect();
    }
    let by_klass = schemas
        .iter()
        .enumerate()
        .map(|(i, s)| (s.klass.clone(), i))
        .collect();
    let by_singular = schemas
        .iter()
        .enumerate()
        .map(|(i, s)| (s.singular.clone(), i))
        .collect();
    Registry {
        schemas,
        by_klass,
        by_singular,
    }
});

/// The registered schema of `klass` (fully qualified class name).
pub fn by_klass(klass: &str) -> Option<&'static RegisteredSchema> {
    let r = &*REGISTRY;
    r.by_klass.get(klass).map(|&i| &r.schemas[i])
}

/// `SchemaService#getSchemaBySingularName`.
pub fn by_singular(singular: &str) -> Option<&'static RegisteredSchema> {
    let r = &*REGISTRY;
    r.by_singular.get(singular).map(|&i| &r.schemas[i])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_schema() {
        let s = by_singular("constant").unwrap();
        assert_eq!(s.klass, "org.hisp.dhis.constant.Constant");
        assert_eq!(s.simple_name(), "Constant");
        assert_eq!(s.relative_api_endpoint.as_deref(), Some("/constants"));
        let id = s.property("id").unwrap();
        assert_eq!(id.field_name.as_deref(), Some("uid"));
        assert!(id.persisted && id.readable && id.sortable);
        // collections are keyed by their collection name
        assert!(s.property("translations").is_some());
        assert!(s.property("translation").is_none());
        let name = s.property("name").unwrap();
        assert!(name.can_be_translated());
        assert_eq!(name.translation_key.as_deref(), Some("NAME"));
        assert_eq!(
            s.property("sharing").unwrap().gist_preferences.included,
            crate::GistInclude::False
        );
        assert!(by_klass("org.hisp.dhis.user.User").is_some());
    }
}
