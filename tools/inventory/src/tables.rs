//! Tables -> `docs/inventory/tables.csv`, from two static sources:
//!
//! 1. the ORM mappings — Hibernate `*.hbm.xml` files and JPA-annotated (`@Entity`) classes —
//!    giving which entity maps to which table and columns;
//! 2. a replay of `CREATE TABLE` / `DROP TABLE` / `ALTER TABLE .. RENAME TO` statements in the
//!    Flyway base schema and migrations (SQL and Java), in version order.
//!
//! The replay is textual (no SQL engine), so it is a ledger, not a schema. Introspecting a
//! Java-migrated database is the authoritative check and is tracked in PORT_STATUS.md.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use quick_xml::Reader;
use quick_xml::events::Event;
use regex::Regex;

use crate::eval::Eval;
use crate::java::{Annotation, Expr, JavaIndex, TypeDecl};

pub const COLUMNS: &[&str] = &[
    "table",
    "present_after_migrations",
    "created_in",
    "dropped_in",
    "orm_mapping",
    "orm_entities",
    "orm_sources",
    "orm_columns",
];

#[derive(Debug, Default)]
struct TableInfo {
    kinds: BTreeSet<&'static str>,
    entities: BTreeSet<String>,
    files: BTreeSet<String>,
    columns: BTreeSet<String>,
}

pub fn extract(dhis2: &Path, idx: &JavaIndex) -> Result<Vec<Vec<String>>> {
    let mut hbm = hbm_tables(dhis2)?;
    for (table, info) in jpa_tables(idx, dhis2) {
        let t = hbm.entry(table).or_default();
        t.kinds.extend(info.kinds);
        t.entities.extend(info.entities);
        t.files.extend(info.files);
        t.columns.extend(info.columns);
    }
    let flyway = flyway_tables(dhis2)?;

    let mut names: BTreeSet<String> = hbm.keys().cloned().collect();
    names.extend(flyway.keys().cloned());

    let mut rows = Vec::new();
    for name in names {
        let h = hbm.get(&name);
        let f = flyway.get(&name);
        let join = |s: Option<&BTreeSet<String>>| {
            s.map(|s| s.iter().cloned().collect::<Vec<_>>().join(";"))
                .unwrap_or_default()
        };
        rows.push(vec![
            name.clone(),
            f.map(|f| match (f.present, f.conditional) {
                (true, true) => "conditional".to_string(),
                (p, _) => p.to_string(),
            })
            .unwrap_or_else(|| "unknown".into()),
            f.and_then(|f| f.created.clone()).unwrap_or_default(),
            f.and_then(|f| f.dropped.clone()).unwrap_or_default(),
            h.map(|h| h.kinds.iter().copied().collect::<Vec<_>>().join("+"))
                .unwrap_or_default(),
            join(h.map(|h| &h.entities)),
            join(h.map(|h| &h.files)),
            join(h.map(|h| &h.columns)),
        ]);
    }
    Ok(rows)
}

fn files_with_suffix(root: &Path, suffix: &str) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = walkdir::WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| {
            let n = e.file_name().to_string_lossy();
            !(e.file_type().is_dir() && (n == "test" || n == "target" || n.starts_with('.')))
        })
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file() && e.file_name().to_string_lossy().ends_with(suffix))
        .map(|e| e.into_path())
        .collect();
    v.sort();
    v
}

// ---------------------------------------------------------------------------------------------
// Hibernate mappings
// ---------------------------------------------------------------------------------------------

/// Every `*.hbm.xml` (path relative to `dhis-2/`, content) with its `classpath://` external
/// entities (e.g. `&identifiableProperties;`) expanded and the DOCTYPE removed.
pub fn expanded_hbm_files(dhis2: &Path) -> Result<Vec<(String, String)>> {
    let doctype = Regex::new(r"(?s)<!DOCTYPE.*?(\[.*?\])?\s*>")?;
    let entity_decl = Regex::new(r#"<!ENTITY\s+(\w+)\s+SYSTEM\s+"classpath://([^"]+)"\s*>"#)?;
    let resources: Vec<PathBuf> = files_with_suffix(dhis2, ".hbm");
    let mut out = Vec::new();
    for path in files_with_suffix(dhis2, ".hbm.xml") {
        let raw = std::fs::read_to_string(&path)?;
        let mut xml = raw.clone();
        for c in entity_decl.captures_iter(&raw) {
            let name = &c[1];
            let rel = &c[2];
            let inc = resources
                .iter()
                .find(|p| p.to_string_lossy().ends_with(rel))
                .with_context(|| format!("{}: entity {rel} not found", path.display()))?;
            let content = std::fs::read_to_string(inc)?;
            xml = xml.replace(&format!("&{name};"), &content);
        }
        let xml = doctype.replace(&xml, "").to_string();
        let file = path
            .strip_prefix(dhis2)
            .unwrap_or(&path)
            .display()
            .to_string();
        out.push((file, xml));
    }
    Ok(out)
}

fn hbm_tables(dhis2: &Path) -> Result<BTreeMap<String, TableInfo>> {
    let mut tables: BTreeMap<String, TableInfo> = BTreeMap::new();
    let mut entity_table: HashMap<String, String> = HashMap::new();
    // (child entity class, key column, file) for one-to-many collections without a table.
    let mut pending_fk: Vec<(String, String, String)> = Vec::new();

    for (file, xml) in expanded_hbm_files(dhis2)? {
        parse_hbm(&xml, &file, &mut tables, &mut entity_table, &mut pending_fk)
            .with_context(|| format!("parsing {file}"))?;
    }

    for (entity, column, file) in pending_fk {
        if let Some(t) = entity_table.get(&entity) {
            let info = tables.entry(t.clone()).or_default();
            info.columns.insert(column);
            info.files.insert(file);
        }
    }
    Ok(tables)
}

#[derive(Debug)]
struct Frame {
    tag: String,
    /// Table that columns of this element (and children) go to.
    table: Option<String>,
    /// Entity class for one-to-many collections without their own table.
    one_to_many: Option<String>,
    /// Column for elements whose column comes from a nested `<column name=..>`.
    wants_column: bool,
    got_column: bool,
    fallback_column: Option<String>,
}

fn parse_hbm(
    xml: &str,
    file: &str,
    tables: &mut BTreeMap<String, TableInfo>,
    entity_table: &mut HashMap<String, String>,
    pending_fk: &mut Vec<(String, String, String)>,
) -> Result<()> {
    let mut reader = Reader::from_str(xml);
    let mut stack: Vec<Frame> = Vec::new();
    let mut package = String::new();

    loop {
        let ev = reader.read_event()?;
        let (e, is_empty) = match &ev {
            Event::Start(e) => (e.clone(), false),
            Event::Empty(e) => (e.clone(), true),
            Event::End(_) => {
                if let Some(f) = stack.pop() {
                    finish(&f, &stack, file, tables, pending_fk);
                }
                continue;
            }
            Event::Eof => break,
            _ => continue,
        };
        let tag = e.name().as_ref().to_string();
        let mut attrs: HashMap<String, String> = HashMap::new();
        for a in e.attributes().flatten() {
            attrs.insert(
                a.key.as_ref().to_string(),
                a.normalized_value(quick_xml::XmlVersion::Implicit1_0)?
                    .to_string(),
            );
        }
        let current_table = stack.iter().rev().find_map(|f| f.table.clone());
        let class_name = |n: &str| {
            if n.contains('.') || package.is_empty() {
                n.to_string()
            } else {
                format!("{package}.{n}")
            }
        };

        let mut frame = Frame {
            tag: tag.clone(),
            table: None,
            one_to_many: None,
            wants_column: false,
            got_column: false,
            fallback_column: None,
        };
        match tag.as_str() {
            "hibernate-mapping" => {
                package = attrs.get("package").cloned().unwrap_or_default();
            }
            "class" | "subclass" | "joined-subclass" | "union-subclass" => {
                let entity = attrs
                    .get("name")
                    .or_else(|| attrs.get("entity-name"))
                    .map(|n| class_name(n))
                    .unwrap_or_default();
                let table = attrs
                    .get("table")
                    .map(|t| t.to_lowercase())
                    .or(current_table.clone());
                if let Some(t) = &table {
                    let info = tables.entry(t.clone()).or_default();
                    info.kinds.insert("hbm");
                    info.entities.insert(entity.clone());
                    info.files.insert(file.to_string());
                    entity_table.insert(entity, t.clone());
                }
                frame.table = table;
            }
            "set" | "list" | "bag" | "idbag" | "map" | "array" | "primitive-array" | "join" => {
                if let Some(t) = attrs.get("table") {
                    let t = t.to_lowercase();
                    let info = tables.entry(t.clone()).or_default();
                    info.kinds.insert("hbm");
                    info.files.insert(file.to_string());
                    frame.table = Some(t);
                }
            }
            "one-to-many" => {
                if let Some(parent) = stack.last_mut()
                    && let Some(c) = attrs.get("class")
                {
                    parent.one_to_many = Some(class_name(c));
                }
            }
            "id" | "property" | "many-to-one" | "key" | "list-index" | "index" | "map-key"
            | "element" | "many-to-many" | "key-property" | "key-many-to-one" | "discriminator"
            | "version" | "timestamp" => {
                frame.wants_column = true;
                let explicit = attrs.get("column").cloned();
                let implicit = match tag.as_str() {
                    "id" | "property" | "many-to-one" | "key-property" | "key-many-to-one"
                    | "version" | "timestamp" => attrs.get("name").cloned(),
                    _ => None,
                };
                if attrs.contains_key("formula") {
                    frame.wants_column = false;
                } else if let Some(c) = explicit {
                    frame.got_column = true;
                    add_column(&stack, &frame, &c, file, tables, pending_fk);
                } else {
                    frame.fallback_column = implicit;
                }
            }
            "column" => {
                if let Some(n) = attrs.get("name")
                    && !stack.is_empty()
                {
                    let len = stack.len();
                    stack[len - 1].got_column = true;
                    let (rest, parent) = stack.split_at(len - 1);
                    add_column(rest, &parent[0], n, file, tables, pending_fk);
                }
            }
            _ => {}
        }

        if is_empty {
            finish(&frame, &stack, file, tables, pending_fk);
        } else {
            stack.push(frame);
        }
    }
    Ok(())
}

/// Called when an element closes: emits the implicit column if no explicit one was seen.
fn finish(
    f: &Frame,
    stack: &[Frame],
    file: &str,
    tables: &mut BTreeMap<String, TableInfo>,
    pending_fk: &mut Vec<(String, String, String)>,
) {
    if f.wants_column
        && !f.got_column
        && let Some(c) = &f.fallback_column
    {
        add_column(stack, f, c, file, tables, pending_fk);
    }
}

fn add_column(
    stack: &[Frame],
    f: &Frame,
    column: &str,
    file: &str,
    tables: &mut BTreeMap<String, TableInfo>,
    pending_fk: &mut Vec<(String, String, String)>,
) {
    // Postgres folds unquoted identifiers to lower case.
    let column = column.trim_matches('`').trim_matches('"').to_lowercase();
    // A <key> of a collection without its own table is a FK column on the child entity table.
    if f.tag == "key"
        && let Some(coll) = stack.last()
        && coll.table.is_none()
        && let Some(child) = &coll.one_to_many
    {
        pending_fk.push((child.clone(), column, file.to_string()));
        return;
    }
    let table = f
        .table
        .clone()
        .or_else(|| stack.iter().rev().find_map(|x| x.table.clone()));
    if let Some(t) = table {
        tables.entry(t).or_default().columns.insert(column);
    }
}

// ---------------------------------------------------------------------------------------------
// JPA annotations
// ---------------------------------------------------------------------------------------------

pub(crate) fn has(anns: &[Annotation], name: &str) -> bool {
    anns.iter().any(|a| a.name == name)
}

pub(crate) fn ann<'a>(anns: &'a [Annotation], name: &str) -> Option<&'a Annotation> {
    anns.iter().find(|a| a.name == name)
}

/// Nested annotations in an attribute (single or array).
pub(crate) fn nested(e: Option<&Expr>) -> Vec<&Annotation> {
    match e {
        Some(Expr::Annotation(a)) => vec![a],
        Some(Expr::Array(items)) => items
            .iter()
            .filter_map(|i| match i {
                Expr::Annotation(a) => Some(a.as_ref()),
                _ => None,
            })
            .collect(),
        _ => vec![],
    }
}

struct Jpa<'a> {
    idx: &'a JavaIndex,
    ev: Eval<'a>,
    tables: BTreeMap<String, TableInfo>,
}

impl<'a> Jpa<'a> {
    fn s(&self, a: &Annotation, attr: &str, ctx: &TypeDecl) -> Option<String> {
        a.arg(attr)
            .and_then(|e| self.ev.string(e, ctx, 0).ok())
            .filter(|v| !v.is_empty())
            .map(|v| v.trim_matches('"').to_lowercase())
    }

    fn add(&mut self, table: &str, column: &str, source: &str) {
        let info = self.tables.entry(table.to_string()).or_default();
        info.kinds.insert("jpa");
        info.files.insert(source.to_string());
        info.columns.insert(column.to_lowercase());
    }

    /// `@Table(name)`, inherited from an `@Entity` superclass (single-table default), else the
    /// entity name.
    fn table_of(&self, t: &TypeDecl) -> String {
        let mut cur = t;
        loop {
            if let Some(tb) = ann(&cur.annotations, "Table")
                && let Some(n) = self.s(tb, "name", cur)
            {
                return n;
            }
            match cur
                .superclass
                .as_deref()
                .and_then(|s| self.idx.resolve_type(cur, s))
            {
                Some(sup) if has(&sup.annotations, "Entity") => cur = sup,
                _ => break,
            }
        }
        ann(&t.annotations, "Entity")
            .and_then(|e| self.s(e, "name", t))
            .unwrap_or_else(|| t.simple_name.to_lowercase())
    }

    fn entity(&mut self, t: &'a TypeDecl, root: &Path) {
        let table = self.table_of(t);
        let source = t
            .file
            .strip_prefix(root)
            .unwrap_or(&t.file)
            .display()
            .to_string();
        {
            let info = self.tables.entry(table.clone()).or_default();
            info.kinds.insert("jpa");
            info.entities.insert(t.fqn.clone());
            info.files.insert(source.clone());
        }
        // The class itself plus @MappedSuperclass ancestors.
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
        for owner in chain {
            for f in &owner.fields {
                self.field(owner, f, &table, &source, &[]);
            }
        }
    }

    fn field(
        &mut self,
        owner: &'a TypeDecl,
        f: &crate::java::Field,
        table: &str,
        source: &str,
        overrides: &[(String, String)],
    ) {
        let a = &f.annotations;
        if f.is_static || f.is_transient || has(a, "Transient") || has(a, "Formula") {
            return;
        }
        let order_col = ann(a, "OrderColumn").and_then(|o| self.s(o, "name", owner));

        if let Some(jt) = ann(a, "JoinTable") {
            if let Some(name) = self.s(jt, "name", owner) {
                for jc in nested(jt.arg("joinColumns"))
                    .into_iter()
                    .chain(nested(jt.arg("inverseJoinColumns")))
                {
                    if let Some(c) = self.s(jc, "name", owner) {
                        self.add(&name, &c, source);
                    }
                }
                if let Some(o) = &order_col {
                    self.add(&name, o, source);
                }
            }
            return;
        }
        if let Some(ct) = ann(a, "CollectionTable") {
            if let Some(name) = self.s(ct, "name", owner) {
                for jc in nested(ct.arg("joinColumns")) {
                    if let Some(c) = self.s(jc, "name", owner) {
                        self.add(&name, &c, source);
                    }
                }
                if let Some(c) = ann(a, "Column").and_then(|c| self.s(c, "name", owner)) {
                    self.add(&name, &c, source);
                }
                if let Some(o) = &order_col {
                    self.add(&name, o, source);
                }
            }
            return;
        }
        let mapped_by = ["OneToMany", "OneToOne", "ManyToMany"]
            .iter()
            .any(|n| ann(a, n).is_some_and(|x| x.arg("mappedBy").is_some()));
        if mapped_by || has(a, "OneToMany") || has(a, "ManyToMany") || has(a, "ElementCollection") {
            return;
        }
        if has(a, "Embedded") || has(a, "EmbeddedId") {
            let mut ov: Vec<(String, String)> = overrides.to_vec();
            let mut list = Vec::new();
            if let Some(x) = ann(a, "AttributeOverride") {
                list.push(x);
            }
            if let Some(x) = ann(a, "AttributeOverrides") {
                list.extend(nested(x.arg("value")));
            }
            for o in list {
                if let (Some(n), Some(c)) = (
                    o.arg("name").and_then(|e| self.ev.string(e, owner, 0).ok()),
                    nested(o.arg("column"))
                        .first()
                        .and_then(|c| self.s(c, "name", owner)),
                ) {
                    ov.push((n, c));
                }
            }
            let ty = crate::java::erase(&f.ty);
            if let Some(emb) = self.idx.resolve_type(owner, &ty) {
                for ef in &emb.fields {
                    self.field(emb, ef, table, source, &ov);
                }
            }
            return;
        }
        if let Some((_, c)) = overrides.iter().find(|(n, _)| *n == f.name) {
            self.add(table, c, source);
            return;
        }
        if let Some(jc) = ann(a, "JoinColumn") {
            let c = self
                .s(jc, "name", owner)
                .unwrap_or_else(|| format!("{}_id", f.name));
            self.add(table, &c, source);
            return;
        }
        let col = ann(a, "Column");
        let target = col
            .and_then(|c| self.s(c, "table", owner))
            .unwrap_or_else(|| table.to_string());
        let name = col
            .and_then(|c| self.s(c, "name", owner))
            .unwrap_or_else(|| f.name.clone());
        self.add(&target, &name, source);
    }
}

fn jpa_tables(idx: &JavaIndex, root: &Path) -> BTreeMap<String, TableInfo> {
    let mut jpa = Jpa {
        idx,
        ev: Eval::new(idx),
        tables: BTreeMap::new(),
    };
    for t in &idx.types {
        if has(&t.annotations, "Entity") {
            jpa.entity(t, root);
        }
    }
    jpa.tables
}

// ---------------------------------------------------------------------------------------------
// Flyway
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Default, Clone)]
struct FlywayState {
    present: bool,
    /// Created inside a `DO $$ ... $$` block, i.e. only when some data condition holds.
    conditional: bool,
    created: Option<String>,
    dropped: Option<String>,
}

fn version_of(name: &str) -> Option<Vec<u64>> {
    let v = name.strip_prefix('V')?.split("__").next()?;
    v.split(['_', '.']).map(|p| p.parse().ok()).collect()
}

fn flyway_tables(dhis2: &Path) -> Result<BTreeMap<String, FlywayState>> {
    let module = dhis2.join("dhis-support/dhis-support-db-migration/src/main");
    let base = module.join("resources/org/hisp/dhis/db/base/dhis2_base_schema.sql");

    let mut migrations: Vec<(Vec<u64>, String, PathBuf)> = Vec::new();
    for dir in [
        module.join("resources/org/hisp/dhis/db/migration"),
        module.join("java/org/hisp/dhis/db/migration"),
    ] {
        for ext in [".sql", ".java"] {
            for p in files_with_suffix(&dir, ext) {
                let name = p.file_stem().unwrap().to_string_lossy().to_string();
                if let Some(v) = version_of(&name) {
                    migrations.push((v, name, p));
                }
            }
        }
    }
    migrations.sort();

    let id = r#"(?:"?public"?\.)?"?([A-Za-z_][A-Za-z0-9_]*)"?"#;
    let create = Regex::new(&format!(
        r"(?i)\bcreate\s+(?:unlogged\s+)?table\s+(?:if\s+not\s+exists\s+)?{id}"
    ))?;
    let drop = Regex::new(&format!(
        r"(?i)\bdrop\s+table\s+(?:if\s+exists\s+)?((?:{id}\s*,\s*)*{id})"
    ))?;
    let drop_item = Regex::new(id)?;
    let rename = Regex::new(&format!(
        r"(?i)\balter\s+table\s+(?:if\s+exists\s+)?(?:only\s+)?{id}\s+rename\s+to\s+{id}"
    ))?;
    let line_comment = Regex::new(r"--[^\n]*")?;

    let mut state: BTreeMap<String, FlywayState> = BTreeMap::new();
    let mut apply = |label: &str, sql: &str| {
        let sql = line_comment.replace_all(sql, "");
        let blocks = dollar_quoted_ranges(&sql);
        let in_block = |pos: usize| blocks.iter().any(|(a, b)| *a <= pos && pos < *b);
        // Collect (position, action) so statements apply in file order.
        let mut actions: Vec<(usize, u8, String, String)> = Vec::new();
        for c in create.captures_iter(&sql) {
            actions.push((
                c.get(0).unwrap().start(),
                0,
                c[1].to_lowercase(),
                String::new(),
            ));
        }
        for c in drop.captures_iter(&sql) {
            for t in drop_item.captures_iter(&c[1]) {
                actions.push((
                    c.get(0).unwrap().start(),
                    1,
                    t[1].to_lowercase(),
                    String::new(),
                ));
            }
        }
        for c in rename.captures_iter(&sql) {
            actions.push((
                c.get(0).unwrap().start(),
                2,
                c[1].to_lowercase(),
                c[2].to_lowercase(),
            ));
        }
        actions.sort();
        for (pos, kind, a, b) in actions {
            match kind {
                0 => {
                    let s = state.entry(a).or_default();
                    if !s.present {
                        s.present = true;
                        s.conditional = in_block(pos);
                        s.created = Some(label.to_string());
                        s.dropped = None;
                    }
                }
                1 => {
                    if let Some(s) = state.get_mut(&a)
                        && s.present
                    {
                        s.present = false;
                        s.dropped = Some(label.to_string());
                    }
                }
                _ => {
                    if let Some(s) = state.get_mut(&a)
                        && s.present
                    {
                        s.present = false;
                        s.dropped = Some(format!("{label} (renamed to {b})"));
                        let n = state.entry(b).or_default();
                        n.present = true;
                        n.created = Some(format!("{label} (renamed from {a})"));
                        n.dropped = None;
                    }
                }
            }
        }
    };

    if let Ok(sql) = std::fs::read_to_string(&base) {
        apply("base_schema", &sql);
    }
    for (_, name, path) in &migrations {
        let src = std::fs::read_to_string(path)?;
        apply(name, &src);
    }
    // Present in every migrated database but not created by migration SQL.
    for (table, origin) in [
        ("flyway_schema_history", "Flyway itself"),
        ("spatial_ref_sys", "PostGIS extension"),
    ] {
        state.insert(
            table.into(),
            FlywayState {
                present: true,
                conditional: false,
                created: Some(origin.into()),
                dropped: None,
            },
        );
    }
    Ok(state)
}

/// Byte ranges of dollar-quoted bodies (`$$ ... $$`, `$tag$ ... $tag$`).
fn dollar_quoted_ranges(sql: &str) -> Vec<(usize, usize)> {
    let tag = Regex::new(r"\$([A-Za-z_][A-Za-z0-9_]*)?\$").expect("valid regex");
    let mut out = Vec::new();
    let mut pos = 0;
    while let Some(open) = tag.find_at(sql, pos) {
        let t = open.as_str();
        match sql[open.end()..].find(t) {
            Some(rel) => {
                let close = open.end() + rel;
                out.push((open.end(), close));
                pos = close + t.len();
            }
            None => break,
        }
    }
    out
}
