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

    /// The Java enum constant name.
    pub fn name(&self) -> &str {
        match self {
            Self::Identifier => "IDENTIFIER",
            Self::Text => "TEXT",
            Self::Boolean => "BOOLEAN",
            Self::Integer => "INTEGER",
            Self::Number => "NUMBER",
            Self::Date => "DATE",
            Self::Reference => "REFERENCE",
            Self::Complex => "COMPLEX",
            Self::Collection => "COLLECTION",
            Self::Other(s) => s,
        }
    }

    /// `Enum#ordinal()` in `org.hisp.dhis.schema.PropertyType` (`None` for a name Java does not
    /// declare).
    pub fn ordinal(&self) -> Option<usize> {
        const ORDER: [&str; 18] = [
            "IDENTIFIER",
            "TEXT",
            "NUMBER",
            "INTEGER",
            "BOOLEAN",
            "USERNAME",
            "EMAIL",
            "PASSWORD",
            "URL",
            "DATE",
            "PHONENUMBER",
            "GEOLOCATION",
            "COLOR",
            "CONSTANT",
            "COMPLEX",
            "COLLECTION",
            "REFERENCE",
            "DEFAULT",
        ];
        ORDER.iter().position(|n| *n == self.name())
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
    // The members below are only in registered schemas (`/api/schemas`); dynamic ones default.
    #[serde(default)]
    pub readable: bool,
    #[serde(default)]
    pub sortable: bool,
    #[serde(default)]
    pub translatable: bool,
    #[serde(default)]
    pub translation_key: Option<String>,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub one_to_one: bool,
    #[serde(default)]
    pub many_to_one: bool,
    #[serde(default)]
    pub one_to_many: bool,
    #[serde(default)]
    pub many_to_many: bool,
    #[serde(default)]
    pub owning_role: Option<String>,
    #[serde(default)]
    pub identifiable_object: bool,
    #[serde(default)]
    pub gist_preferences: GistPreferences,
}

/// `org.hisp.dhis.schema.GistPreferences` (`DEFAULT`: included and transformation `AUTO`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GistPreferences {
    #[serde(default)]
    pub included: GistInclude,
    #[serde(default)]
    pub transformation: Option<String>,
    #[serde(default)]
    pub order: Option<i32>,
}

/// `GistPreferences.Flag`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum GistInclude {
    #[default]
    Auto,
    True,
    False,
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

    /// `Property#key()`: the name properties are looked up by (`collectionName` for collections).
    pub fn key(&self) -> &str {
        self.output_name()
    }

    /// `Property#isRelation()`.
    pub fn is_relation(&self) -> bool {
        self.one_to_one || self.one_to_many || self.many_to_one || self.many_to_many
    }

    /// `Property#canBeTranslated()`.
    pub fn can_be_translated(&self) -> bool {
        self.translatable && self.translation_key.is_some()
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
    dynamic!("org.hisp.dhis.schema.Schema"),
    dynamic!("org.hisp.dhis.schema.Property"),
    dynamic!("org.hisp.dhis.schema.GistPreferences"),
    dynamic!("org.hisp.dhis.security.Authority"),
    dynamic!("org.hisp.dhis.user.sharing.Sharing"),
    dynamic!("org.hisp.dhis.user.sharing.UserAccess"),
    dynamic!("org.hisp.dhis.user.sharing.UserGroupAccess"),
    dynamic!("org.hisp.dhis.security.acl.Access"),
    dynamic!("org.hisp.dhis.security.acl.AccessData"),
    dynamic!("org.hisp.dhis.translation.Translation"),
    dynamic!("org.hisp.dhis.schema.transformer.UserPropertyTransformer$UserDto"),
];

static SCHEMAS: LazyLock<BTreeMap<&'static str, Schema>> = LazyLock::new(|| {
    DYNAMIC
        .iter()
        .map(|(klass, json)| {
            let mut raw: BTreeMap<String, serde_json::Value> = serde_json::from_str(json)
                .unwrap_or_else(|e| panic!("bad generated schema for {klass}: {e}"));
            // DefaultSchemaService#updateSelf drops the `__schema__` pseudo-property (from
            // `@JacksonXmlRootElement`) from every schema, dynamic ones included.
            raw.remove("__schema__");
            let properties: BTreeMap<String, Property> = raw
                .into_iter()
                .map(|(k, v)| {
                    let p = serde_json::from_value(v)
                        .unwrap_or_else(|e| panic!("bad generated property {klass}.{k}: {e}"));
                    (k, p)
                })
                .collect();
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

pub mod hibernate;
pub mod jackson;
pub mod registered;

/// `Property.resolveTranslationBasePropertyName`: `displayX` → `x` (first letter lower-cased).
pub fn resolve_translation_base_property_name(name: &str) -> String {
    match name.strip_prefix("display") {
        Some(rest) if !rest.is_empty() => {
            let mut chars = rest.chars();
            let first = chars.next().map(|c| c.to_lowercase().to_string());
            format!("{}{}", first.unwrap_or_default(), chars.as_str())
        }
        _ => name.to_string(),
    }
}

/// The registered schemas (`SchemaService#getSortedSchemas`) as Java renders them, captured from
/// the pinned build by `tools/gen-registered-schemas.sh` (ADR 0007). Links (`href`,
/// `apiEndpoint`) carry the placeholder base [`CAPTURE_API_PATH`].
pub const REGISTERED_SCHEMAS_JSON: &str = include_str!("../data/registered/schemas.json");

/// The API path the registered schemas were captured with; replaced by the request's API path.
pub const CAPTURE_API_PATH: &str = "http://rust-capture.invalid/api";

/// `SchemaService#getSchema(Class)`. Classes Java would describe with a property-less dynamic
/// schema (JDK types such as `java.lang.String`, `java.util.Map`) have none here (`None`).
pub fn schema(klass: &str) -> Option<&'static Schema> {
    SCHEMAS
        .get(klass)
        .or_else(|| REGISTERED_AS_SCHEMAS.get(klass))
}

/// The registered schemas in the shape of [`Schema`] (properties by `Property#key()`).
static REGISTERED_AS_SCHEMAS: LazyLock<BTreeMap<String, Schema>> = LazyLock::new(|| {
    let all: Vec<registered::RegisteredSchema> =
        serde_json::from_str(REGISTERED_SCHEMAS_JSON).unwrap_or_default();
    all.into_iter()
        .map(|s| {
            let properties = s
                .properties
                .iter()
                .map(|p| (p.key().to_string(), p.clone()))
                .collect();
            (
                s.klass.clone(),
                Schema {
                    klass: s.klass,
                    properties,
                },
            )
        })
        .collect()
});

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
