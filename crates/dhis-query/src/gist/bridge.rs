//! `GistBridge#isSupportedObjectList`: whether a metadata list request (`/api/{type}`) is
//! answered by the Gist engine.
//!
//! Only a [`Decision::Gist`] answer is acted on: anything else goes to Java, which then decides
//! itself, so an uncertain case may only err towards [`Decision::Unknown`].

use dhis_schema::registered::{self, RegisteredSchema};
use dhis_schema::{Property, resolve_translation_base_property_name};

use super::{Filter, Order};
use crate::fields::Fields;
use crate::property_path::PropertyPath;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Gist,
    /// The metadata query engine answers (not ported).
    Metadata,
    /// Not decidable here (types without registered schema, ...).
    Unknown,
}

/// The bridge's inputs (`GistBridgeParams`).
#[derive(Debug, Clone)]
pub struct BridgeInput<'a> {
    pub schema: &'static RegisteredSchema,
    /// `gist` parameter: explicit opt-in/out.
    pub gist: Option<bool>,
    pub paging: bool,
    /// All request parameter names.
    pub param_names: Vec<&'a str>,
    /// `getFieldsJsonList()`: `fields` or the default `id,displayName`.
    pub fields: &'a str,
    /// `filter` / `order` values joined with commas.
    pub filter: &'a str,
    pub order: &'a str,
}

/// `GIST_BRIDE_PARAMS`.
const BRIDGE_PARAMS: [&str; 9] = [
    "fields",
    "filter",
    "order",
    "page",
    "pageSize",
    "paging",
    "locale",
    "rootJunction",
    "gist",
];

struct Unknown;

/// `RelativePropertyContext#resolvePath` (empty when a segment does not exist).
fn property_path(
    home: &'static RegisteredSchema,
    path: &PropertyPath,
) -> Result<Vec<&'static Property>, Unknown> {
    if !path.is_nested() {
        return Ok(home.property(path.property()).into_iter().collect());
    }
    let mut schema = home;
    let mut out = Vec::new();
    let segments = path.segments();
    for (i, seg) in segments.iter().enumerate() {
        if seg.starts_with([':', '!', '-']) {
            return Err(Unknown);
        }
        let Some(p) = schema.property(seg) else {
            return Ok(Vec::new());
        };
        out.push(p);
        if i + 1 < segments.len() {
            let klass = if p.collection {
                &p.item_klass
            } else {
                &p.klass
            };
            // Java builds dynamic schemas for other classes; their properties are not known here
            schema = klass
                .as_deref()
                .and_then(registered::by_klass)
                .ok_or(Unknown)?;
        }
    }
    Ok(out)
}

fn is_translated_property(home: &'static RegisteredSchema, p: &Property) -> Result<bool, Unknown> {
    if !p.name.starts_with("display") {
        return Ok(false);
    }
    let base = resolve_translation_base_property_name(&p.name);
    let path = PropertyPath::of(&base).map_err(|_| Unknown)?;
    Ok(property_path(home, &path)?
        .last()
        .is_some_and(|b| b.translatable && b.persisted))
}

fn supported_field(home: &'static RegisteredSchema, path: &PropertyPath) -> Result<bool, Unknown> {
    if path.is_preset() {
        return Ok(false);
    }
    let props = property_path(home, path)?;
    if props.is_empty() || props.len() > 2 || props.iter().any(|p| p.embedded_object) {
        return Ok(false);
    }
    if props.len() == 2 {
        let r = props[0];
        let ref_type = if r.collection {
            &r.item_klass
        } else {
            &r.klass
        };
        let identifiable = ref_type
            .as_deref()
            .and_then(registered::by_klass)
            .ok_or(Unknown)?
            .identifiable_object;
        if !identifiable {
            return Ok(false);
        }
    }
    let leaf = props[props.len() - 1];
    if leaf.collection {
        return Ok(false);
    }
    if !leaf.persisted {
        return Ok(props.len() == 1 && is_translated_property(home, leaf)?);
    }
    Ok(leaf.simple || leaf.klass.as_deref() == Some("org.hisp.dhis.user.sharing.Sharing"))
}

fn decide_inner(input: &BridgeInput<'_>) -> Result<bool, Unknown> {
    if let Some(gist) = input.gist {
        return Ok(gist);
    }
    if !input.paging || input.schema.klass == "org.hisp.dhis.attribute.Attribute" {
        return Ok(false);
    }
    if input.param_names.iter().any(|n| !BRIDGE_PARAMS.contains(n)) {
        return Ok(false);
    }
    if input.fields.is_empty() {
        return Ok(false);
    }
    // Java catches RuntimeExceptions of the whole check as "not supported"; parse errors here
    // can also be cases Java parses, hence Unknown.
    let fields = Fields::of(input.fields).map_err(|_| Unknown)?;
    for f in &fields.0 {
        if !supported_field(input.schema, &f.property_path)? {
            return Ok(false);
        }
    }
    if input.filter.contains("token") || input.filter.contains("display") {
        return Ok(false);
    }
    for f in Filter::of_list(input.filter).map_err(|_| Unknown)? {
        let props = property_path(input.schema, &f.path)?;
        if props.len() != 1 || !props[0].persisted || props[0].embedded_object {
            return Ok(false);
        }
    }
    if ["iasc", "idesc", "IASC", "IDESC"]
        .iter()
        .any(|s| input.order.contains(s))
    {
        return Ok(false);
    }
    for o in Order::of_list(input.order).map_err(|_| Unknown)? {
        let props = property_path(input.schema, &o.path)?;
        if props.len() != 1 {
            return Ok(false);
        }
        let p = props[0];
        let ok = !p.embedded_object
            && p.simple
            && p.sortable
            && (p.persisted || is_translated_property(input.schema, p)?);
        if !ok {
            return Ok(false);
        }
    }
    Ok(true)
}

pub fn decide(input: &BridgeInput<'_>) -> Decision {
    match decide_inner(input) {
        Ok(true) => Decision::Gist,
        Ok(false) => Decision::Metadata,
        Err(Unknown) => Decision::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input<'a>(names: Vec<&'a str>, fields: &'a str) -> BridgeInput<'a> {
        BridgeInput {
            schema: registered::by_singular("constant").unwrap(),
            gist: None,
            paging: true,
            param_names: names,
            fields,
            filter: "",
            order: "",
        }
    }

    #[test]
    fn decisions_like_java() {
        assert_eq!(decide(&input(vec![], "id,displayName")), Decision::Gist);
        assert_eq!(decide(&input(vec![], "*")), Decision::Metadata);
        assert_eq!(decide(&input(vec![], "id,sharing")), Decision::Gist);
        assert_eq!(decide(&input(vec![], "id,access")), Decision::Metadata);
        assert_eq!(
            decide(&input(vec![], "id,translations")),
            Decision::Metadata
        );
        assert_eq!(
            decide(&input(vec![], "id,lastUpdatedBy")),
            Decision::Metadata
        );
        assert_eq!(
            decide(&input(vec![], "id,lastUpdatedBy.name")),
            Decision::Gist
        );
        assert_eq!(decide(&input(vec!["query"], "id")), Decision::Metadata);
        let mut i = input(vec!["fields", "order"], "id");
        i.order = "name:iasc";
        assert_eq!(decide(&i), Decision::Metadata);
        i.order = "displayName:desc";
        assert_eq!(decide(&i), Decision::Gist);
        i.filter = "displayName:eq:x";
        assert_eq!(decide(&i), Decision::Metadata);
        i.filter = "value:gt:1";
        assert_eq!(decide(&i), Decision::Gist);
        i.paging = false;
        assert_eq!(decide(&i), Decision::Metadata);
        i.gist = Some(true);
        assert_eq!(decide(&i), Decision::Gist);
    }
}
