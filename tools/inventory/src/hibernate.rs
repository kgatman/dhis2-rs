//! Hibernate `*.hbm.xml` mappings -> `crates/dhis-schema/data/hibernate/hbm-entities.json`.
//!
//! Rust queries the shared database with its own SQL where Java uses HQL (e.g. the Gist API). The
//! HQL names properties; this table gives, per mapped class, the table, the identifier column and
//! the column (or relation) of each direct property, exactly as the mapping files declare them.
//!
//! Only what the mapping says is recorded. Column names are folded to lower case as PostgreSQL
//! folds unquoted identifiers, and a property without `column` maps to a column named like the
//! property (Hibernate's default; DHIS2 configures no naming strategy). Anything not modelled
//! (collections, components, joins, formulas, subclasses) is recorded by kind only, so callers can
//! tell "not supported" from "does not exist".
//!
//! JPA-annotated entities are not covered yet.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use quick_xml::Reader;
use quick_xml::events::Event;
use serde_json::{Value, json};

use crate::tables::expanded_hbm_files;

pub fn extract(dhis2: &Path) -> Result<String> {
    let mut entities: BTreeMap<String, Value> = BTreeMap::new();
    for (file, xml) in expanded_hbm_files(dhis2)? {
        parse(&xml, &file, &mut entities).with_context(|| format!("parsing {file}"))?;
    }
    let mut out = serde_json::to_string_pretty(&entities)?;
    out.push('\n');
    Ok(out)
}

struct Open {
    tag: String,
    /// Index into `classes` when this element is a class-like element.
    class: Option<usize>,
    /// The property being built when this element is a direct property of a class.
    property: Option<(usize, String, serde_json::Map<String, Value>)>,
}

struct Class {
    name: String,
    record: serde_json::Map<String, Value>,
    properties: BTreeMap<String, Value>,
}

fn lower(column: &str) -> String {
    column.trim_matches('`').trim_matches('"').to_lowercase()
}

fn parse(xml: &str, file: &str, entities: &mut BTreeMap<String, Value>) -> Result<()> {
    let mut reader = Reader::from_str(xml);
    let mut stack: Vec<Open> = Vec::new();
    let mut classes: Vec<Class> = Vec::new();
    let mut package = String::new();

    loop {
        let ev = reader.read_event()?;
        let (e, is_empty) = match &ev {
            Event::Start(e) => (e.clone(), false),
            Event::Empty(e) => (e.clone(), true),
            Event::End(_) => {
                if let Some(o) = stack.pop() {
                    close(o, &mut classes);
                }
                continue;
            }
            Event::Eof => break,
            _ => continue,
        };
        let tag = e.name().as_ref().to_string();
        let mut attrs: BTreeMap<String, String> = BTreeMap::new();
        for a in e.attributes().flatten() {
            attrs.insert(
                a.key.as_ref().to_string(),
                a.normalized_value(quick_xml::XmlVersion::Implicit1_0)?
                    .to_string(),
            );
        }
        let class_name = |n: &str| {
            if n.contains('.') || package.is_empty() {
                n.to_string()
            } else {
                format!("{package}.{n}")
            }
        };
        let parent_class = stack.last().and_then(|o| o.class);
        let mut open = Open {
            tag: tag.clone(),
            class: None,
            property: None,
        };
        match tag.as_str() {
            "hibernate-mapping" => {
                package = attrs.get("package").cloned().unwrap_or_default();
            }
            "class" | "subclass" | "joined-subclass" | "union-subclass" => {
                let name = attrs
                    .get("name")
                    .or_else(|| attrs.get("entity-name"))
                    .map(|n| class_name(n))
                    .unwrap_or_default();
                let mut record = serde_json::Map::new();
                record.insert("file".into(), json!(file));
                record.insert("mapping".into(), json!(tag));
                if let Some(t) = attrs.get("table") {
                    record.insert("table".into(), json!(lower(t)));
                }
                let parent = parent_class
                    .map(|i| classes[i].name.clone())
                    .or_else(|| attrs.get("extends").map(|n| class_name(n)));
                if let Some(p) = parent {
                    record.insert("extends".into(), json!(p));
                }
                if let Some(i) = parent_class {
                    classes[i]
                        .record
                        .insert("hasSubclasses".into(), json!(true));
                }
                classes.push(Class {
                    name,
                    record,
                    properties: BTreeMap::new(),
                });
                open.class = Some(classes.len() - 1);
            }
            "discriminator" => {
                if let Some(i) = parent_class {
                    classes[i]
                        .record
                        .insert("discriminator".into(), json!(true));
                }
            }
            _ => {
                if let Some(ci) = parent_class
                    && let Some(name) = attrs.get("name")
                {
                    let mut p = serde_json::Map::new();
                    p.insert("kind".into(), json!(tag));
                    match tag.as_str() {
                        "id" | "property" | "many-to-one" | "one-to-one" | "version"
                        | "timestamp" => {
                            if attrs.contains_key("formula") {
                                p.insert("formula".into(), json!(true));
                            } else if tag != "one-to-one" {
                                // Default column: the property name (a nested <column> wins).
                                let column = attrs.get("column").unwrap_or(name);
                                p.insert("column".into(), json!(lower(column)));
                            }
                            if let Some(t) = attrs.get("type") {
                                p.insert("type".into(), json!(t));
                            }
                            if let Some(c) = attrs.get("class") {
                                p.insert("class".into(), json!(class_name(c)));
                            }
                            if attrs.get("not-null").map(String::as_str) == Some("true") {
                                p.insert("notNull".into(), json!(true));
                            }
                        }
                        _ => {}
                    }
                    open.property = Some((ci, name.clone(), p));
                }
            }
        }
        if tag == "column"
            && let Some(top) = stack.last_mut()
            && let Some((_, _, p)) = top.property.as_mut()
            && let Some(n) = attrs.get("name")
            && p.contains_key("column")
        {
            p.insert("column".into(), json!(lower(n)));
            if attrs.get("not-null").map(String::as_str) == Some("true") {
                p.insert("notNull".into(), json!(true));
            }
        }
        if is_empty {
            close(open, &mut classes);
        } else {
            stack.push(open);
        }
    }
    for c in classes {
        let mut record = c.record;
        record.insert("properties".into(), json!(c.properties));
        entities.insert(c.name, Value::Object(record));
    }
    Ok(())
}

fn close(o: Open, classes: &mut [Class]) {
    let _ = o.tag;
    if let Some((ci, name, p)) = o.property {
        if p.get("kind").and_then(Value::as_str) == Some("id") {
            classes[ci].record.insert(
                "id".into(),
                json!({"name": name, "column": p.get("column").cloned().unwrap_or(Value::Null)}),
            );
        } else {
            classes[ci].properties.insert(name, Value::Object(p));
        }
    }
}
