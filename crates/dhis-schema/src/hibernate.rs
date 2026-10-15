//! Hibernate mappings (hbm and JPA-annotated classes), generated from the pinned Java source by
//! `dhis-inventory` (`data/hibernate/hbm-entities.json`, drift-checked in CI): the table, the
//! identifier column and the column or relation behind each property name used in HQL.
//! Every recorded column was checked against a Java-migrated database (all present).

use std::collections::BTreeMap;
use std::sync::LazyLock;

use serde::Deserialize;

const HBM_ENTITIES_JSON: &str = include_str!("../data/hibernate/hbm-entities.json");

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entity {
    /// `class`, `subclass`, `joined-subclass` or `union-subclass`.
    pub mapping: String,
    #[serde(default)]
    pub table: Option<String>,
    #[serde(default)]
    pub id: Option<Id>,
    #[serde(default)]
    pub extends: Option<String>,
    #[serde(default)]
    pub has_subclasses: bool,
    #[serde(default)]
    pub discriminator: bool,
    #[serde(default)]
    pub composite_id: bool,
    pub properties: BTreeMap<String, MappedProperty>,
}

impl Entity {
    /// A plain class (hbm `<class>` or JPA `@Entity`) mapped to one table, without inheritance:
    /// the only shape whose SQL is derived directly so far.
    pub fn is_plain_class(&self) -> bool {
        (self.mapping == "class" || self.mapping == "jpa")
            && !self.composite_id
            && self.extends.is_none()
            && !self.has_subclasses
            && !self.discriminator
            && self.table.is_some()
            && self.id.is_some()
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Id {
    pub name: String,
    pub column: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MappedProperty {
    /// The mapping element: `property`, `many-to-one`, `set`, `component`, ...
    pub kind: String,
    #[serde(default)]
    pub column: Option<String>,
    /// The Hibernate `type` attribute, if any (e.g. `text`, `timestamp`, `jsbObjectSharing`).
    #[serde(default, rename = "type")]
    pub hibernate_type: Option<String>,
    /// Target class of a `many-to-one`/`one-to-one`.
    #[serde(default)]
    pub class: Option<String>,
    #[serde(default)]
    pub formula: bool,
    /// Collections: the join table, the owner's key column and the element class/column.
    #[serde(default)]
    pub table: Option<String>,
    #[serde(default)]
    pub key_column: Option<String>,
    #[serde(default)]
    pub element_class: Option<String>,
    #[serde(default)]
    pub element_column: Option<String>,
    #[serde(default)]
    pub inverse: bool,
    /// The (secondary) table a join column lives in, when not the entity's own.
    #[serde(default)]
    pub column_table: Option<String>,
    /// JPA `mappedBy`: the owning side's property on the element class.
    #[serde(default)]
    pub mapped_by: Option<String>,
    /// JPA embeddable collapsed to its single column.
    #[serde(default)]
    pub embedded: bool,
    #[serde(default)]
    pub not_null: bool,
}

static ENTITIES: LazyLock<BTreeMap<String, Entity>> = LazyLock::new(|| {
    serde_json::from_str(HBM_ENTITIES_JSON)
        .unwrap_or_else(|e| panic!("bad hibernate mapping data: {e}"))
});

/// The hbm mapping of `klass`, if it is mapped by a `*.hbm.xml` file.
pub fn entity(klass: &str) -> Option<&'static Entity> {
    ENTITIES.get(klass)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_mapping() {
        let e = entity("org.hisp.dhis.constant.Constant").unwrap();
        assert!(e.is_plain_class());
        assert_eq!(e.table.as_deref(), Some("constant"));
        assert_eq!(e.id.as_ref().unwrap().column, "constantid");
        let created_by = &e.properties["createdBy"];
        assert_eq!(created_by.kind, "many-to-one");
        assert_eq!(created_by.column.as_deref(), Some("userid"));
        assert_eq!(
            e.properties["lastUpdated"].column.as_deref(),
            Some("lastupdated")
        );
    }
}
