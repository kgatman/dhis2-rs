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
//! JPA-annotated (`@Entity`) classes are added in the same shape (`mapping: "jpa"`), from their
//! fields and `@MappedSuperclass` ancestors; see [`extract_with_jpa`].

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use quick_xml::Reader;
use quick_xml::events::Event;
use serde_json::{Value, json};

use crate::tables::expanded_hbm_files;

/// `EnumType` parameters: named (`useNamed`) or VARCHAR (`type` 12) values are stored as text.
fn enum_param(p: &mut serde_json::Map<String, Value>, name: &str, value: &str) {
    let is_enum = p
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|t| t.starts_with("enum-"));
    if is_enum && ((name == "useNamed" && value == "true") || (name == "type" && value == "12")) {
        p.insert("type".into(), json!("enum-string"));
    }
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

    // `<param name="x">value</param>` inside a property's `<type>`: (name, owner stack depth)
    let mut param: Option<String> = None;
    loop {
        let ev = reader.read_event()?;
        let (e, is_empty) = match &ev {
            Event::Start(e) => (e.clone(), false),
            Event::Empty(e) => (e.clone(), true),
            Event::Text(t) => {
                if let Some(name) = param.take()
                    && let Some(top) = stack.iter_mut().rev().find(|o| o.property.is_some())
                    && let Some((_, _, p)) = top.property.as_mut()
                {
                    let value = t[..].trim().to_string();
                    enum_param(p, &name, &value);
                }
                continue;
            }
            Event::End(_) => {
                param = None;
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
                        "set" | "list" | "bag" | "idbag" | "map" | "array" => {
                            if let Some(t) = attrs.get("table") {
                                p.insert("table".into(), json!(lower(t)));
                            }
                            if attrs.get("inverse").map(String::as_str) == Some("true") {
                                p.insert("inverse".into(), json!(true));
                            }
                        }
                        _ => {}
                    }
                    open.property = Some((ci, name.clone(), p));
                }
            }
        }
        // A collection's join: <key column> (owner side) and the element <many-to-many> or
        // <one-to-many>.
        // list order (<list-index>/<index>) and plain values (<element>)
        if matches!(tag.as_str(), "list-index" | "index" | "element")
            && let Some(top) = stack.last_mut()
            && let Some((_, _, p)) = top.property.as_mut()
            && p.contains_key("kind")
            && !p.contains_key("column")
            && let Some(c) = attrs.get("column")
        {
            if tag == "element" {
                p.insert("elementColumn".into(), json!(lower(c)));
                if let Some(t) = attrs.get("type") {
                    p.insert("elementType".into(), json!(t));
                }
            } else {
                p.insert("indexColumn".into(), json!(lower(c)));
            }
        }
        if matches!(tag.as_str(), "key" | "many-to-many" | "one-to-many")
            && let Some(top) = stack.last_mut()
            && let Some((_, _, p)) = top.property.as_mut()
            && p.contains_key("kind")
            && !p.contains_key("column")
        {
            match tag.as_str() {
                "key" => {
                    if let Some(c) = attrs.get("column") {
                        p.insert("keyColumn".into(), json!(lower(c)));
                    }
                }
                _ => {
                    if let Some(c) = attrs.get("class") {
                        let c = if c.contains('.') || package.is_empty() {
                            c.clone()
                        } else {
                            format!("{package}.{c}")
                        };
                        p.insert("elementClass".into(), json!(c));
                    }
                    if let Some(c) = attrs.get("column") {
                        p.insert("elementColumn".into(), json!(lower(c)));
                    }
                }
            }
        }
        // nested <type name="..."> (e.g. Hibernate's EnumType) and its <param>s
        if tag == "type"
            && let Some(top) = stack.last_mut()
            && let Some((_, _, p)) = top.property.as_mut()
            && let Some(n) = attrs.get("name")
        {
            p.insert(
                "type".into(),
                json!(if n == "org.hibernate.type.EnumType" {
                    "enum-ordinal"
                } else {
                    n.as_str()
                }),
            );
        }
        if tag == "param" {
            param = attrs.get("name").cloned();
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

// ---------------------------------------------------------------------------------------------
// JPA-annotated entities
// ---------------------------------------------------------------------------------------------

use crate::eval::Eval;
use crate::java::{Annotation, Expr, JavaIndex, TypeDecl};
use crate::tables::{ann, has, nested};

/// Adds the `@Entity` classes, in the same shape as the hbm ones. Columns follow JPA defaults
/// where no `@Column`/`@JoinColumn` name is given (field name; `{field}_id` for joins), folded to
/// lower case like unquoted identifiers.
pub fn extract_with_jpa(dhis2: &Path, idx: &JavaIndex) -> Result<String> {
    let mut entities: BTreeMap<String, Value> = BTreeMap::new();
    for (file, xml) in expanded_hbm_files(dhis2)? {
        parse(&xml, &file, &mut entities).with_context(|| format!("parsing {file}"))?;
    }
    // custom types extending EnumUserType store Enum#name(): the same as enum-string
    let enum_user_types: std::collections::BTreeSet<String> = idx
        .types
        .iter()
        .filter(|t| {
            t.superclass
                .as_deref()
                .is_some_and(|s| s.starts_with("EnumUserType"))
        })
        .map(|t| t.fqn.clone())
        .collect();
    for e in entities.values_mut() {
        if let Some(Value::Object(props)) = e.get_mut("properties") {
            for p in props.values_mut() {
                if let Some(t) = p.get("type").and_then(Value::as_str)
                    && enum_user_types.contains(t)
                {
                    p["type"] = json!("enum-string");
                }
            }
        }
    }
    let jpa = JpaEntities {
        idx,
        ev: Eval::new(idx),
    };
    let subclassed: std::collections::BTreeSet<String> = idx
        .types
        .iter()
        .filter(|t| has(&t.annotations, "Entity"))
        .filter_map(|t| t.superclass.as_deref().and_then(|s| idx.resolve_type(t, s)))
        .filter(|s| has(&s.annotations, "Entity"))
        .map(|s| s.fqn.clone())
        .collect();
    for t in &idx.types {
        if has(&t.annotations, "Entity") && !entities.contains_key(&t.fqn) {
            let mut e = jpa.entity(t, dhis2);
            if subclassed.contains(&t.fqn)
                && let Value::Object(m) = &mut e
            {
                m.insert("hasSubclasses".into(), json!(true));
            }
            entities.insert(t.fqn.clone(), e);
        }
    }
    let mut out = serde_json::to_string_pretty(&entities)?;
    out.push('\n');
    Ok(out)
}

struct JpaEntities<'a> {
    idx: &'a JavaIndex,
    ev: Eval<'a>,
}

impl<'a> JpaEntities<'a> {
    fn s(&self, a: &Annotation, attr: &str, ctx: &TypeDecl) -> Option<String> {
        a.arg(attr)
            .and_then(|e| self.ev.string(e, ctx, 0).ok())
            .filter(|v| !v.is_empty())
    }

    fn col(&self, a: &Annotation, attr: &str, ctx: &TypeDecl) -> Option<String> {
        self.s(a, attr, ctx).map(|v| lower(&v))
    }

    fn entity(&self, t: &'a TypeDecl, root: &Path) -> Value {
        let mut record = serde_json::Map::new();
        let file = t
            .file
            .strip_prefix(root)
            .unwrap_or(&t.file)
            .display()
            .to_string();
        record.insert("file".into(), json!(file));
        record.insert("mapping".into(), json!("jpa"));
        // @Entity superclass: inheritance (table per the strategy), recorded as such
        let entity_parent = t
            .superclass
            .as_deref()
            .and_then(|s| self.idx.resolve_type(t, s))
            .filter(|s| has(&s.annotations, "Entity"));
        if let Some(p) = entity_parent {
            record.insert("extends".into(), json!(p.fqn));
        }
        if has(&t.annotations, "Inheritance") || has(&t.annotations, "DiscriminatorColumn") {
            record.insert("discriminator".into(), json!(true));
        }
        let table = ann(&t.annotations, "Table")
            .and_then(|tb| self.col(tb, "name", t))
            .or_else(|| {
                entity_parent
                    .is_none()
                    .then(|| t.simple_name.to_lowercase())
            });
        if let Some(tb) = table {
            record.insert("table".into(), json!(tb));
        }
        // @SecondaryTable: its key column (the @PrimaryKeyJoinColumn, by default the primary
        // key's column name, resolved by readers)
        let mut secondary = serde_json::Map::new();
        for st in t.annotations.iter().filter(|a| a.name == "SecondaryTable") {
            if let Some(name) = self.col(st, "name", t) {
                let key = nested(st.arg("pkJoinColumns"))
                    .first()
                    .and_then(|c| self.col(c, "name", t));
                secondary.insert(name, json!(key));
            }
        }
        if !secondary.is_empty() {
            record.insert("secondaryTables".into(), Value::Object(secondary));
        }
        let mut chain = vec![t];
        let mut cur = t;
        while let Some(sup) = cur
            .superclass
            .as_deref()
            .and_then(|s| self.idx.resolve_type(cur, s))
        {
            if !has(&sup.annotations, "MappedSuperclass") {
                break;
            }
            chain.push(sup);
            cur = sup;
        }
        let mut properties: BTreeMap<String, Value> = BTreeMap::new();
        for owner in chain {
            for f in &owner.fields {
                if f.is_static || f.is_transient || has(&f.annotations, "Transient") {
                    continue;
                }
                if has(&f.annotations, "Id") {
                    if record.contains_key("id") {
                        // several @Id fields: a composite identifier
                        record.remove("id");
                        record.insert("compositeId".into(), json!(true));
                        continue;
                    }
                    if record.contains_key("compositeId") {
                        continue;
                    }
                    let column = ann(&f.annotations, "Column")
                        .and_then(|c| self.col(c, "name", owner))
                        .unwrap_or_else(|| lower(&f.name));
                    record.insert("id".into(), json!({"name": f.name, "column": column}));
                    continue;
                }
                if let Some(p) = self.field(owner, f) {
                    properties.entry(f.name.clone()).or_insert(p);
                }
            }
        }
        record.insert("properties".into(), json!(properties));
        Value::Object(record)
    }

    fn class_of(&self, owner: &TypeDecl, ty: &str) -> Option<String> {
        let erased = crate::java::erase(ty);
        self.idx.resolve_type(owner, &erased).map(|t| t.fqn.clone())
    }

    /// The element type name of `Set<X>`/`List<X>` as written (e.g. `Integer`).
    fn element_type(&self, _owner: &TypeDecl, ty: &str) -> Option<String> {
        Some(
            ty.split_once('<')?
                .1
                .trim_end_matches('>')
                .trim()
                .to_string(),
        )
    }

    /// The element type of `Set<X>`/`List<X>`.
    fn element_of(&self, owner: &TypeDecl, ty: &str) -> Option<String> {
        let inner = ty.split_once('<')?.1.trim_end_matches('>').trim();
        self.class_of(owner, inner)
    }

    fn field(&self, owner: &TypeDecl, f: &crate::java::Field) -> Option<Value> {
        let a = &f.annotations;
        let mut p = serde_json::Map::new();
        if has(a, "Formula") {
            p.insert("kind".into(), json!("property"));
            p.insert("formula".into(), json!(true));
            return Some(Value::Object(p));
        }
        let join_col = |p: &mut serde_json::Map<String, Value>| {
            let jc = ann(a, "JoinColumn");
            let c = jc
                .and_then(|j| self.col(j, "name", owner))
                .unwrap_or_else(|| lower(&format!("{}_id", f.name)));
            p.insert("column".into(), json!(c));
            // a join column kept in a secondary table
            if let Some(t) = jc.and_then(|j| self.col(j, "table", owner)) {
                p.insert("columnTable".into(), json!(t));
            }
            if jc.and_then(|j| j.arg("nullable")) == Some(&Expr::Bool(false)) {
                p.insert("notNull".into(), json!(true));
            }
        };
        for (anno, kind) in [("ManyToOne", "many-to-one"), ("OneToOne", "one-to-one")] {
            if let Some(x) = ann(a, anno) {
                p.insert("kind".into(), json!(kind));
                if let Some(c) = self.class_of(owner, &f.ty) {
                    p.insert("class".into(), json!(c));
                }
                if x.arg("mappedBy").is_some() {
                    p.insert("inverse".into(), json!(true));
                } else {
                    join_col(&mut p);
                }
                if x.arg("optional") == Some(&Expr::Bool(false)) {
                    p.insert("notNull".into(), json!(true));
                }
                return Some(Value::Object(p));
            }
        }
        let collection_kind = if crate::java::erase(&f.ty).ends_with("List") {
            "list"
        } else {
            "set"
        };
        for anno in ["ManyToMany", "OneToMany"] {
            if let Some(x) = ann(a, anno) {
                p.insert("kind".into(), json!(collection_kind));
                if let Some(c) = self.element_of(owner, &f.ty) {
                    p.insert("elementClass".into(), json!(c));
                }
                if x.arg("mappedBy").is_some() {
                    p.insert("inverse".into(), json!(true));
                    if let Some(m) = self.s(x, "mappedBy", owner) {
                        p.insert("mappedBy".into(), json!(m));
                    }
                }
                // a one-to-many with a plain @JoinColumn: the key column is on the item table
                if ann(a, "JoinTable").is_none()
                    && x.arg("mappedBy").is_none()
                    && let Some(c) = ann(a, "JoinColumn").and_then(|j| self.col(j, "name", owner))
                {
                    p.insert("keyColumn".into(), json!(c));
                }
                if let Some(c) = ann(a, "OrderColumn").and_then(|o| self.col(o, "name", owner)) {
                    p.insert("indexColumn".into(), json!(c));
                }
                if let Some(o) = ann(a, "OrderBy").and_then(|o| self.s(o, "value", owner)) {
                    p.insert("orderBy".into(), json!(o));
                }
                if let Some(jt) = ann(a, "JoinTable") {
                    if let Some(t) = self.col(jt, "name", owner) {
                        p.insert("table".into(), json!(t));
                    }
                    if let Some(c) = nested(jt.arg("joinColumns"))
                        .first()
                        .and_then(|c| self.col(c, "name", owner))
                    {
                        p.insert("keyColumn".into(), json!(c));
                    }
                    if let Some(c) = nested(jt.arg("inverseJoinColumns"))
                        .first()
                        .and_then(|c| self.col(c, "name", owner))
                    {
                        p.insert("elementColumn".into(), json!(c));
                    }
                }
                return Some(Value::Object(p));
            }
        }
        if has(a, "ElementCollection") {
            p.insert("kind".into(), json!("element-collection"));
            if let Some(ct) = ann(a, "CollectionTable") {
                if let Some(t) = self.col(ct, "name", owner) {
                    p.insert("table".into(), json!(t));
                }
                if let Some(c) = nested(ct.arg("joinColumns"))
                    .first()
                    .and_then(|c| self.col(c, "name", owner))
                {
                    p.insert("keyColumn".into(), json!(c));
                }
            }
            if let Some(c) = ann(a, "Column").and_then(|c| self.col(c, "name", owner)) {
                p.insert("elementColumn".into(), json!(c));
            }
            if let Some(c) = ann(a, "OrderColumn").and_then(|o| self.col(o, "name", owner)) {
                p.insert("indexColumn".into(), json!(c));
            }
            if let Some(t) = self.element_type(owner, &f.ty) {
                p.insert("elementType".into(), json!(t));
            }
            return Some(Value::Object(p));
        }
        if has(a, "Embedded") || has(a, "EmbeddedId") {
            // An embeddable with one persistent field (e.g. TranslationProperty) is that column.
            let emb = self.idx.resolve_type(owner, &crate::java::erase(&f.ty))?;
            let fields: Vec<&crate::java::Field> = emb
                .fields
                .iter()
                .filter(|x| !x.is_static && !x.is_transient && !has(&x.annotations, "Transient"))
                .collect();
            if let [only] = fields.as_slice()
                && let Some(Value::Object(mut inner)) = self.field(emb, only)
            {
                inner.insert("embedded".into(), json!(true));
                return Some(Value::Object(inner));
            }
            p.insert("kind".into(), json!("component"));
            return Some(Value::Object(p));
        }
        p.insert("kind".into(), json!("property"));
        let col = ann(a, "Column");
        let column = col
            .and_then(|c| self.col(c, "name", owner))
            .unwrap_or_else(|| lower(&f.name));
        p.insert("column".into(), json!(column));
        if col.and_then(|c| c.arg("nullable")) == Some(&Expr::Bool(false)) {
            p.insert("notNull".into(), json!(true));
        }
        let ty = if let Some(t) = ann(a, "Type") {
            self.s(t, "type", owner)
                .or_else(|| self.s(t, "value", owner))
        } else if let Some(e) = ann(a, "Enumerated") {
            Some(match e.arg("value") {
                Some(Expr::Name(n)) if n.last().map(String::as_str) == Some("STRING") => {
                    "enum-string".to_string()
                }
                _ => "enum-ordinal".to_string(),
            })
        } else if ann(a, "Convert").is_some() {
            Some("converted".to_string())
        } else if let Some(c) = col.and_then(|c| self.s(c, "columnDefinition", owner)) {
            Some(c.to_lowercase())
        } else {
            ann(a, "Temporal").map(|t| {
                match t.arg("value") {
                    Some(Expr::Name(n)) if n.last().map(String::as_str) == Some("DATE") => "date",
                    Some(Expr::Name(n)) if n.last().map(String::as_str) == Some("TIME") => "time",
                    _ => "timestamp",
                }
                .to_string()
            })
        };
        if let Some(t) = ty {
            p.insert("type".into(), json!(t));
        }
        Some(Value::Object(p))
    }
}
