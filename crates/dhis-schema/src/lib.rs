//! Schema/property metadata, as Java's `SchemaService` computes it.
//!
//! Java counterpart: dhis-service-schema (`org.hisp.dhis.schema`).
//!
//! The metadata is not hand-written: it is generated from the pinned Java build.
//! - Dynamic schemas (classes without a registered descriptor, built at runtime by
//!   `DefaultSchemaService#createDynamicSchema`) come from `tools/gen-dynamic-schemas.sh`, which
//!   runs Java's property introspectors and writes `data/dynamic/<class>.json`.
//! - Registered metadata schemas (`/api/schemas`) will be added the same way.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use serde::Deserialize;

/// `org.hisp.dhis.schema.PropertyType` (only the distinctions the code relies on are explicit).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(from = "String")]
pub enum PropertyType {
    Identifier,
    Text,
    Boolean,
    Integer,
    Number,
    Date,
    Reference,
    Complex,
    Collection,
    /// Any other Java value (`EMAIL`, `GEOLOCATION`, `CONSTANT`, ...), kept verbatim.
    Other(String),
}

impl From<String> for PropertyType {
    fn from(s: String) -> Self {
        match s.as_str() {
            "IDENTIFIER" => Self::Identifier,
            "TEXT" => Self::Text,
            "BOOLEAN" => Self::Boolean,
            "INTEGER" => Self::Integer,
            "NUMBER" => Self::Number,
            "DATE" => Self::Date,
            "REFERENCE" => Self::Reference,
            "COMPLEX" => Self::Complex,
            "COLLECTION" => Self::Collection,
            _ => Self::Other(s),
        }
    }
}

impl PropertyType {
    /// `PropertyType#isSimple()`: neither reference, complex nor collection.
    pub fn is_simple(&self) -> bool {
        !matches!(self, Self::Reference | Self::Complex | Self::Collection)
    }
}

/// `org.hisp.dhis.schema.Property` (fields used by field filtering so far).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Property {
    pub name: String,
    #[serde(default)]
    pub field_name: Option<String>,
    #[serde(default)]
    pub collection_name: Option<String>,
    #[serde(default)]
    pub klass: Option<String>,
    #[serde(default)]
    pub item_klass: Option<String>,
    pub property_type: PropertyType,
    #[serde(default, deserialize_with = "nullable_type")]
    pub item_property_type: Option<PropertyType>,
    pub collection: bool,
    pub simple: bool,
    pub owner: bool,
    pub persisted: bool,
    pub embedded_object: bool,
    #[serde(default)]
    pub property_transformer: bool,
}

fn nullable_type<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<PropertyType>, D::Error> {
    let s: Option<String> = Option::deserialize(d)?;
    Ok(s.filter(|s| s != "null").map(PropertyType::from))
}

impl Property {
    /// The name the property has in output: `collectionName` for collections.
    pub fn output_name(&self) -> &str {
        if self.collection {
            self.collection_name.as_deref().unwrap_or(&self.name)
        } else {
            &self.name
        }
    }

    pub fn is(&self, t: &PropertyType) -> bool {
        self.property_type == *t
    }

    pub fn item_is(&self, t: &PropertyType) -> bool {
        self.item_property_type.as_ref() == Some(t)
    }

    /// The class whose schema describes this property's value (item class for collections).
    pub fn value_klass(&self) -> Option<&str> {
        if self.collection {
            self.item_klass.as_deref()
        } else {
            self.klass.as_deref()
        }
    }
}

#[derive(Debug, Clone)]
pub struct Schema {
    pub klass: String,
    /// By `Property#name`. Java iterates a `HashMap`; order of these never reaches output
    /// (field filtering only selects, output order is the serialisation order).
    pub properties: BTreeMap<String, Property>,
}

impl Schema {
    pub fn property(&self, name: &str) -> Option<&Property> {
        self.properties.get(name)
    }
}

macro_rules! dynamic {
    ($class:literal) => {
        (
            $class,
            include_str!(concat!("../data/dynamic/", $class, ".json")),
        )
    };
}

const DYNAMIC: &[(&str, &str)] = &[
    dynamic!("org.hisp.dhis.system.SystemInfo"),
    dynamic!("org.hisp.dhis.system.database.DatabaseInfo"),
    dynamic!("org.hisp.dhis.system.capability.SystemCapability"),
];

static SCHEMAS: LazyLock<BTreeMap<&'static str, Schema>> = LazyLock::new(|| {
    DYNAMIC
        .iter()
        .map(|(klass, json)| {
            let properties: BTreeMap<String, Property> = serde_json::from_str(json)
                .unwrap_or_else(|e| panic!("bad generated schema for {klass}: {e}"));
            (
                *klass,
                Schema {
                    klass: klass.to_string(),
                    properties,
                },
            )
        })
        .collect()
});

/// `SchemaService#getSchema(Class)`. Classes Java would describe with a property-less dynamic
/// schema (JDK types such as `java.lang.String`, `java.util.Map`) have none here (`None`).
pub fn schema(klass: &str) -> Option<&'static Schema> {
    SCHEMAS.get(klass)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_schemas_load() {
        let s = schema("org.hisp.dhis.system.SystemInfo").unwrap();
        assert_eq!(s.properties.len(), 50);
        let db = s.property("databaseInfo").unwrap();
        assert_eq!(db.property_type, PropertyType::Complex);
        assert_eq!(
            db.value_klass(),
            Some("org.hisp.dhis.system.database.DatabaseInfo")
        );
        assert!(s.property("version").unwrap().simple);
        assert!(schema("org.hisp.dhis.system.database.DatabaseInfo").is_some());
    }
}
