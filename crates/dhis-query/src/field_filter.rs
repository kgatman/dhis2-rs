//! Port of `org.hisp.dhis.fieldfiltering` (dhis-service-field-filtering): applying a parsed
//! `fields=` expression to a response object.
//!
//! Java serialises the object with Jackson and a property filter
//! (`FieldFilterSimpleBeanPropertyFilter`) that keeps a bean property only if its dotted path is
//! in the include set computed by `FieldPathHelper#apply` from the requested paths and the class
//! schemas. Here, handlers render a typed [`Node`] tree in Jackson's property order; [`filter`]
//! then keeps what Jackson would keep and [`Node::write_json`] writes it out in that order.
//!
//! Not ported yet (callers must fall back to Java): field transformers (`rename`, `size`,
//! `isEmpty`, `isNotEmpty`, `pluck`, `keyBy`), `access`/sharing display names and attribute
//! values for identifiable objects.

use std::collections::{BTreeMap, BTreeSet};

use dhis_schema::{Property, PropertyType, Schema};

use crate::fields::FieldPath;
use crate::property_path::PropertyPath;

/// A rendered value, as Jackson sees it.
#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    /// A POJO; its properties go through the field filter. `klass` is the Java class name used
    /// to find its schema.
    Bean {
        klass: &'static str,
        props: Vec<(String, Node)>,
    },
    /// A `java.util.Map`: always expanded, never filtered.
    Map(Vec<(String, Node)>),
    Array(Vec<Node>),
    Scalar(serde_json::Value),
}

impl Node {
    /// Compact JSON as Jackson writes it, keeping property order.
    pub fn write_json(&self, out: &mut String) {
        match self {
            Node::Bean { props, .. } | Node::Map(props) => {
                out.push('{');
                for (i, (k, v)) in props.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    dhis_core_model::java::json::write_string(out, k);
                    out.push(':');
                    v.write_json(out);
                }
                out.push('}');
            }
            Node::Array(items) => {
                out.push('[');
                for (i, v) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    v.write_json(out);
                }
                out.push(']');
            }
            Node::Scalar(v) => out.push_str(&dhis_core_model::java::json::to_string(v)),
        }
    }

    pub fn to_json_string(&self) -> String {
        let mut s = String::new();
        self.write_json(&mut s);
        s
    }
}

/// Why a request cannot be served by this engine yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unsupported {
    Transformers,
}

const SHARING_LIKE: &[&str] = &[
    "org.hisp.dhis.user.sharing.Sharing",
    "org.hisp.dhis.security.acl.Access",
    "org.hisp.dhis.user.sharing.UserAccess",
    "org.hisp.dhis.user.sharing.UserGroupAccess",
];

fn is_reference(p: &Property) -> bool {
    p.is(&PropertyType::Reference) || p.item_is(&PropertyType::Reference)
}

/// `FieldPathHelper#isComplex`. Java uses `isAssignableFrom` for the sharing classes; DHIS2 has
/// no subclasses of them.
fn is_complex(p: &Property) -> bool {
    p.is(&PropertyType::Complex)
        || p.item_is(&PropertyType::Complex)
        || p.embedded_object
        || p.klass
            .as_deref()
            .is_some_and(|k| SHARING_LIKE.contains(&k))
}

/// `FieldPreset.IDENTIFIABLE` / `FieldPreset.NAMEABLE` field names.
const IDENTIFIABLE: &[&str] = &[
    "id",
    "name",
    "code",
    "created",
    "lastUpdated",
    "lastUpdatedBy",
];
const NAMEABLE: &[&str] = &[
    "id",
    "name",
    "shortName",
    "description",
    "code",
    "created",
    "lastUpdated",
    "lastUpdatedBy",
];

struct Inclusion {
    path: PropertyPath,
    property: Option<&'static Property>,
    transformers: bool,
}

/// `FieldPathHelper#getSchemaByPath`.
fn schema_by_path(path: Option<&PropertyPath>, root: &'static Schema) -> Option<&'static Schema> {
    let mut schema = root;
    let Some(path) = path else {
        return Some(schema);
    };
    for seg in path.segments() {
        let name = seg.trim_start_matches(['!', '-']);
        let prop = schema.property(name)?;
        schema = dhis_schema::schema(prop.value_klass()?)?;
    }
    Some(schema)
}

fn to_inclusion(parent: Option<&PropertyPath>, p: &'static Property) -> Option<Inclusion> {
    let path = PropertyPath::concat_opt(parent, p.output_name()).ok()?;
    Some(Inclusion {
        path,
        property: Some(p),
        transformers: false,
    })
}

/// `FieldPathHelper#apply`: the effective paths for `paths` on objects of class `root`.
fn apply(paths: &[FieldPath], root: &'static Schema) -> BTreeMap<String, Inclusion> {
    let mut inclusions: BTreeMap<String, Inclusion> = BTreeMap::new();
    for p in paths
        .iter()
        .filter(|p| !p.path.is_preset() && !p.path.is_exclude())
    {
        inclusions.insert(
            p.path.to_string(),
            Inclusion {
                path: p.path.clone(),
                property: None,
                transformers: !p.transformers.is_empty(),
            },
        );
    }

    // applyProperties
    for inc in inclusions.values_mut() {
        if let Some(schema) = schema_by_path(inc.path.parent().as_ref(), root) {
            inc.property = schema.property(inc.path.property());
        }
    }

    // applyPresets
    for preset in paths.iter().filter(|p| p.path.is_preset()) {
        let parent = preset.path.parent();
        let Some(schema) = schema_by_path(parent.as_ref(), root) else {
            continue;
        };
        let name = preset
            .path
            .segments()
            .last()
            .map(String::as_str)
            .unwrap_or("");
        let keep: fn(&Property) -> bool = match name {
            ":all" => |_| true,
            ":owner" => |p| p.owner,
            ":persisted" => |p| p.persisted,
            ":identifiable" => |p| IDENTIFIABLE.contains(&p.name.as_str()),
            ":simple" => |p| p.property_type.is_simple(),
            ":nameable" => |p| NAMEABLE.contains(&p.name.as_str()),
            _ => continue,
        };
        for p in schema.properties.values().filter(|p| keep(p)) {
            if let Some(inc) = to_inclusion(parent.as_ref(), p) {
                inclusions.entry(inc.path.to_string()).or_insert(inc);
            }
        }
    }

    // calculatePathCount + applyDefaults for paths counted at most once
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for inc in inclusions.values() {
        let Some(property) = inc.property else {
            continue;
        };
        let mut parent = inc.path.parent();
        while let Some(p) = parent {
            *counts.entry(p.to_string()).or_default() += 1;
            parent = p.parent();
        }
        if is_reference(property) || is_complex(property) {
            *counts.entry(inc.path.to_string()).or_default() += 1;
        }
    }
    let defaults: Vec<(PropertyPath, &'static Property)> = counts
        .iter()
        .filter(|(_, c)| **c <= 1)
        .filter_map(|(k, _)| inclusions.get(k))
        .filter_map(|inc| inc.property.map(|p| (inc.path.clone(), p)))
        .collect();
    for (path, property) in defaults {
        let Some(schema) = schema_by_path(Some(&path), root) else {
            continue;
        };
        if is_complex(property) {
            expand_complex(&mut inclusions, &path, schema);
        } else if is_reference(property) {
            expand_reference(&mut inclusions, &path, schema);
        }
    }

    // applyExclusions
    for ex in paths.iter().filter(|p| p.path.is_exclude()) {
        inclusions.retain(|_, inc| !is_excluded_by(&inc.path, &ex.path));
    }

    // pruneTransformerControlledSubtrees
    let pruned: BTreeSet<String> = inclusions
        .values()
        .filter(|inc| beneath_property_transformer(&inc.path, root))
        .map(|inc| inc.path.to_string())
        .collect();
    inclusions.retain(|k, _| !pruned.contains(k));
    inclusions
}

/// `FieldPathHelper#applyDefault` (recursive part of `expandComplex`).
fn apply_default(inclusions: &mut BTreeMap<String, Inclusion>, inc: Inclusion) {
    let property = inc.property;
    let path = inc.path.clone();
    inclusions.insert(path.to_string(), inc);
    let Some(property) = property else { return };
    if property.simple {
        return;
    }
    let Some(schema) = property.value_klass().and_then(dhis_schema::schema) else {
        return;
    };
    if is_complex(property) {
        expand_complex(inclusions, &path, schema);
    } else if is_reference(property) {
        expand_reference(inclusions, &path, schema);
    }
}

fn expand_complex(
    inclusions: &mut BTreeMap<String, Inclusion>,
    parent: &PropertyPath,
    schema: &'static Schema,
) {
    for p in schema.properties.values() {
        if let Some(inc) = to_inclusion(Some(parent), p) {
            apply_default(inclusions, inc);
        }
    }
}

fn expand_reference(
    inclusions: &mut BTreeMap<String, Inclusion>,
    parent: &PropertyPath,
    schema: &'static Schema,
) {
    if let Some(id) = schema.property("id")
        && let Some(inc) = to_inclusion(Some(parent), id)
    {
        inclusions.insert(inc.path.to_string(), inc);
    }
}

/// `PropertyPath#isExcluded` via `FieldPathHelper#fieldShouldBeExcluded`: the exclusion's
/// properties equal the field path's leading properties.
fn is_excluded_by(field: &PropertyPath, exclusion: &PropertyPath) -> bool {
    if !exclusion.is_exclude() || exclusion.len() > field.len() {
        return false;
    }
    let strip = |s: &str| s.trim_start_matches(['!', '-']).to_string();
    exclusion
        .segments()
        .iter()
        .zip(field.segments())
        .all(|(a, b)| strip(a) == strip(b))
}

fn beneath_property_transformer(path: &PropertyPath, root: &'static Schema) -> bool {
    let mut ancestor = path.parent();
    while let Some(a) = ancestor {
        let schema = schema_by_path(a.parent().as_ref(), root);
        if schema
            .and_then(|s| s.property(a.property()))
            .is_some_and(|p| p.property_transformer)
        {
            return true;
        }
        ancestor = a.parent();
    }
    false
}

/// The include set of dotted property paths (exclude markers dropped), as handed to Jackson.
pub fn include_paths(
    paths: &[FieldPath],
    root_klass: &str,
) -> Result<BTreeSet<String>, Unsupported> {
    let Some(root) = dhis_schema::schema(root_klass) else {
        return Ok(BTreeSet::new());
    };
    let inclusions = apply(paths, root);
    if inclusions.values().any(|i| i.transformers) {
        return Err(Unsupported::Transformers);
    }
    Ok(inclusions
        .values()
        .map(|i| {
            i.path
                .segments()
                .iter()
                .map(|s| s.trim_start_matches(['!', '-']))
                .collect::<Vec<_>>()
                .join(".")
        })
        .collect())
}

/// `FieldFilterService#toObjectNode`: keeps what Jackson + `FieldFilterSimpleBeanPropertyFilter`
/// would serialise.
pub fn filter(node: &Node, includes: &BTreeSet<String>) -> Node {
    filter_at(node, includes, "", false)
}

fn join(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{prefix}.{name}")
    }
}

fn filter_at(node: &Node, includes: &BTreeSet<String>, prefix: &str, expand: bool) -> Node {
    match node {
        Node::Bean { klass, props } => Node::Bean {
            klass,
            props: props
                .iter()
                .filter_map(|(name, value)| {
                    let path = join(prefix, name);
                    (expand || includes.contains(&path))
                        .then(|| (name.clone(), filter_at(value, includes, &path, expand)))
                })
                .collect(),
        },
        // Maps are "always expand" types: everything beneath them is kept.
        Node::Map(entries) => Node::Map(
            entries
                .iter()
                .map(|(k, v)| (k.clone(), filter_at(v, includes, &join(prefix, k), true)))
                .collect(),
        ),
        // Array contexts carry no name.
        Node::Array(items) => Node::Array(
            items
                .iter()
                .map(|v| filter_at(v, includes, prefix, expand))
                .collect(),
        ),
        Node::Scalar(v) => Node::Scalar(v.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fields::parse;

    const SI: &str = "org.hisp.dhis.system.SystemInfo";

    fn inc(fields: &str) -> BTreeSet<String> {
        include_paths(&parse(fields).unwrap(), SI).unwrap()
    }

    #[test]
    fn all_expands_complex_children() {
        let s = inc("*");
        assert!(s.contains("version"));
        assert!(s.contains("databaseInfo"));
        assert!(s.contains("databaseInfo.name"));
        assert!(s.contains("capability.analyticsTotalCounts"));
    }

    #[test]
    fn explicit_children_do_not_expand() {
        let s = inc("databaseInfo[name]");
        assert!(s.contains("databaseInfo") && s.contains("databaseInfo.name"));
        assert!(!s.contains("databaseInfo.url"));
        let s = inc("databaseInfo");
        assert!(s.contains("databaseInfo.url"));
    }

    #[test]
    fn exclusions_and_simple_preset() {
        let s = inc("*,!databaseInfo");
        assert!(!s.iter().any(|p| p.starts_with("databaseInfo")));
        let s = inc(":simple");
        assert!(s.contains("version") && !s.contains("databaseInfo"));
        let s = inc("databaseInfo[*,!url]");
        assert!(s.contains("databaseInfo.name") && !s.contains("databaseInfo.url"));
    }

    #[test]
    fn transformers_are_reported() {
        assert_eq!(
            include_paths(&parse("version::rename(v)").unwrap(), SI),
            Err(Unsupported::Transformers)
        );
    }

    #[test]
    fn filtering_keeps_order_and_maps() {
        let node = Node::Bean {
            klass: SI,
            props: vec![
                ("contextPath".into(), Node::Scalar("x".into())),
                (
                    "lastAnalyticsTableSuccessByType".into(),
                    Node::Map(vec![("EVENT".into(), Node::Scalar("d".into()))]),
                ),
                (
                    "databaseInfo".into(),
                    Node::Bean {
                        klass: "org.hisp.dhis.system.database.DatabaseInfo",
                        props: vec![
                            ("name".into(), Node::Scalar("dhis".into())),
                            ("url".into(), Node::Scalar("u".into())),
                        ],
                    },
                ),
                ("version".into(), Node::Scalar("2.44".into())),
            ],
        };
        let out = filter(
            &node,
            &inc("version,databaseInfo[name],lastAnalyticsTableSuccessByType"),
        );
        assert_eq!(
            out.to_json_string(),
            r#"{"lastAnalyticsTableSuccessByType":{"EVENT":"d"},"databaseInfo":{"name":"dhis"},"version":"2.44"}"#
        );
    }
}
