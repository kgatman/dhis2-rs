//! The properties DHIS2's JSON mapper writes per class, in serialisation order, as the field
//! filter's copy of `JacksonObjectMapperConfig.jsonMapper` builds its bean serializers
//! (generated on the server image by `tools/gen-jackson-properties.sh`).
//!
//! The order of getter-only properties follows the JVM's reflection order, which may differ
//! between JVM builds (the image's JDK 21 is the reference, deviation R1).

use std::collections::BTreeMap;
use std::sync::LazyLock;

use serde::Deserialize;

const PROPERTIES_JSON: &str = include_str!("../data/jackson/properties.json");

#[derive(Debug, Clone, Deserialize)]
pub struct JacksonProperty {
    pub name: String,
    #[serde(default)]
    pub getter: Option<String>,
    /// A custom serializer's class, if any.
    #[serde(default)]
    pub serializer: Option<String>,
    #[serde(default, rename = "type")]
    pub java_type: Option<String>,
}

static PROPERTIES: LazyLock<BTreeMap<String, Vec<JacksonProperty>>> = LazyLock::new(|| {
    serde_json::from_str(PROPERTIES_JSON)
        .unwrap_or_else(|e| panic!("bad jackson property data: {e}"))
});

/// The written properties of `klass` in order, if generated.
pub fn properties(klass: &str) -> Option<&'static [JacksonProperty]> {
    PROPERTIES.get(klass).map(Vec::as_slice)
}

#[cfg(test)]
mod tests {
    #[test]
    fn constant_order() {
        let names: Vec<&str> = super::properties("org.hisp.dhis.constant.Constant")
            .unwrap()
            .iter()
            .map(|p| p.name.as_str())
            .collect();
        assert_eq!(names.first(), Some(&"code"));
        assert!(names.contains(&"access") && names.contains(&"attributeValues"));
    }
}
