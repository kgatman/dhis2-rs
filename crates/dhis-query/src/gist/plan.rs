//! `GistPlanner`, `GistValidator` and `GistBuilder` for list queries, producing PostgreSQL.
//!
//! Java writes HQL that Hibernate translates; this writes the SQL that translation amounts to,
//! using the generated mappings (`dhis_schema::hibernate`): `e.<property>` becomes `e.<column>`,
//! an entity comparison becomes an identifier-column comparison, `setFirstResult`/`setMaxResults`
//! become `offset`/`limit`.
//!
//! Supported so far (everything else is [`Unsupported`]):
//! - fields: non-nested simple persisted properties, `display*` translations, `href`, persistent
//!   references to identifiable objects with their own endpoint (`apiEndpoints`), presets `*`,
//!   `:all`, `:persisted`, `:owner`, excludes;
//! - filters: non-nested simple persisted properties, all operators but access checks, no
//!   filter groups; values of type text, number, integer and boolean;
//! - orders: simple persisted properties and translations;
//! - plain hbm-mapped classes only; for other users than superusers, readable types only (field
//!   access checks then pass for every field) and the sharing filter on the listed type.

use std::collections::{HashMap, HashSet};

use dhis_schema::hibernate::{self, Entity};
use dhis_schema::registered::{self, RegisteredSchema};
use dhis_schema::{GistInclude, Property, resolve_translation_base_property_name};

use super::{AutoType, Comparison, Filter, Order, Result, Unsupported};
use crate::fields::{Field, Fields, Transform};
use crate::property_path::PropertyPath;

/// Inputs of `GistPipeline#createListQuery` (after parameter binding).
#[derive(Debug, Clone)]
pub struct ListParams {
    /// `fields` as given (empty: all `auto` fields).
    pub fields: String,
    pub filter: String,
    pub order: String,
    pub page: i32,
    pub page_size: i32,
    /// Count the total matches (`totalPages`/`total`).
    pub total: bool,
    pub absolute_urls: bool,
    pub references: bool,
    /// `rootJunction=OR`.
    pub any_filter: bool,
    pub auto: AutoType,
    /// `Locale#toString()` of the translation locale (the `locale` parameter or the user's
    /// database locale).
    pub translation_locale: String,
    /// `getRootPath(request)`: used for absolute URLs.
    pub context_root: String,
}

/// The current user as far as the query depends on it (`DefaultGistAccessControl`).
#[derive(Debug, Clone)]
pub struct Access {
    pub superuser: bool,
    /// `isAuthorized("Test_skipSharingCheck")`: no sharing filter.
    pub skip_sharing_check: bool,
    /// `aclService.canRead(user, elementType)` (the type's `READ` authorities).
    pub can_read_type: bool,
    /// The session's user id and sorted group ids; needed for the sharing filter.
    pub user: Option<(String, Vec<String>)>,
    /// `UserDetails#getAllAuthorities()`, for `aclService.canRead(user, type)` of other types.
    pub authorities: Vec<String>,
}

impl Access {
    pub fn superuser() -> Self {
        Self {
            superuser: true,
            skip_sharing_check: true,
            can_read_type: true,
            user: None,
            authorities: Vec::new(),
        }
    }

    /// `aclService.canRead(user, type)`: superusers, types without `READ` authorities, or any of
    /// them.
    pub fn can_read(&self, schema: &RegisteredSchema) -> bool {
        let read = schema.authorities_by_type("READ");
        self.superuser
            || read.is_empty()
            || read.iter().any(|a| self.authorities.iter().any(|u| u == a))
    }
}

/// A database column value type, from the property's Java class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnType {
    Text,
    Double,
    Int,
    Long,
    Bool,
    /// `text[]`/`varchar[]` (`array_agg`).
    TextArray,
    /// A JSONB column read through one of DHIS2's JSON types, selected as text.
    Json(JsonKind),
    /// `timestamp`; selected as text (`YYYY-MM-DD HH24:MI:SS.US`), rendered as Java renders the
    /// `java.util.Date` it becomes.
    Timestamp,
}

/// JSONB columns and the Java value they are read into (`dhis_core_model::jsonb`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonKind {
    /// `jsbObjectSharing`: `Sharing`, written by Jackson.
    Sharing,
    /// `jblTranslations`: a `Set<Translation>` (hbm) or a `TranslationProperty` (JPA embedded,
    /// written wrapped as `{"translations":[...]}`), written by Jackson.
    Translations { wrapped: bool },
    /// `jsbAttributeValues`: `AttributeValues`, written by itself (json-tree).
    AttributeValues,
}

/// One value per field of the fetch query (Java's `Object[]` row).
#[derive(Debug, Clone)]
pub enum Slot {
    /// Selected by the SQL (in order of the `Sql` slots).
    Sql(ColumnType),
    /// `href`: `{prefix}{id}{suffix}` (`toEndpointURL`) from the same-parent `id` slot; stays
    /// null without an id slot or endpoint, or when the id is null.
    Href {
        id_slot: Option<usize>,
        prefix: Option<String>,
        suffix: String,
    },
    /// `apiEndpoints`: one URL per reference field, keyed by output path.
    Refs { entries: Vec<RefEntry> },
}

#[derive(Debug, Clone)]
pub struct RefEntry {
    /// `field.path()` (`TreeMap` key, so entries are kept sorted).
    pub key: String,
    /// `{root}/{id}/{property}/gist{params}` with `{id}` from this slot.
    pub id_slot: usize,
    pub prefix: String,
    pub suffix: String,
}

/// How an output property is rendered (`ObjectOutput.Type` and its adder).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputType {
    String,
    Date,
    Double,
    Integer,
    Boolean,
    StringMap,
    /// `String[]`: an array of strings.
    StringArray,
    /// `IdObject[]`: `[{"id": ...}]`, null when empty (`toIdObjects`).
    IdObjects,
    /// JSON text already rendered (Jackson-mapped or self-encoding values), inserted as is.
    Raw,
}

#[derive(Debug, Clone)]
pub struct OutputProperty {
    pub path: String,
    pub ty: OutputType,
}

/// A bound SQL parameter.
#[derive(Debug, Clone, PartialEq)]
pub enum SqlParam {
    Text(String),
    Double(f64),
    Int(i32),
    Long(i64),
    Bool(bool),
}

#[derive(Debug, Clone)]
pub struct ListPlan {
    pub schema: &'static RegisteredSchema,
    pub fetch_sql: String,
    /// `None` unless the total is requested.
    pub count_sql: Option<String>,
    pub params: Vec<SqlParam>,
    /// One per field of the fetch query; only the first `output.len()` are rendered.
    pub slots: Vec<Slot>,
    pub output: Vec<OutputProperty>,
    pub page: i32,
    pub page_size: i32,
}

/// `RelativePropertyContext`, over the registered schemas (paths of one or two segments).
struct Ctx {
    home: &'static RegisteredSchema,
}

impl Ctx {
    fn resolve(&self, path: &PropertyPath) -> Result<Option<&'static Property>> {
        Ok(self.resolve_path(path)?.and_then(|ps| ps.last().copied()))
    }

    /// `resolvePath`: the property of each segment (`None` when one does not exist).
    fn resolve_path(&self, path: &PropertyPath) -> Result<Option<Vec<&'static Property>>> {
        let segs = path.segments();
        match segs {
            [_] => Ok(self.home.property(path.property()).map(|p| vec![p])),
            [head, leaf] => {
                if head.starts_with([':', '!', '-']) || leaf.starts_with([':', '!', '-']) {
                    return Err(Unsupported("nested modifier"));
                }
                let Some(h) = self.home.property(head) else {
                    return Ok(None);
                };
                let klass = if h.collection {
                    &h.item_klass
                } else {
                    &h.klass
                };
                let target = klass
                    .as_deref()
                    .and_then(registered::by_klass)
                    .ok_or(Unsupported("nested in unregistered type"))?;
                Ok(target.property(leaf).map(|l| vec![h, l]))
            }
            _ => Err(Unsupported("deeply nested path")),
        }
    }

    /// The schema owning the last segment (`switchedTo(path).getHome()`).
    fn owner_of(&self, path: &PropertyPath) -> Result<&'static RegisteredSchema> {
        match path.segments() {
            [_] => Ok(self.home),
            [head, _] => {
                let h = self
                    .home
                    .property(head)
                    .ok_or(Unsupported("no such property"))?;
                let klass = if h.collection {
                    &h.item_klass
                } else {
                    &h.klass
                };
                klass
                    .as_deref()
                    .and_then(registered::by_klass)
                    .ok_or(Unsupported("nested in unregistered type"))
            }
            _ => Err(Unsupported("deeply nested path")),
        }
    }

    fn resolve_name(&self, name: &str) -> Result<Option<&'static Property>> {
        let path = PropertyPath::of(name).map_err(|_| Unsupported("invalid property path"))?;
        self.resolve(&path)
    }

    fn resolve_mandatory(&self, path: &PropertyPath) -> Result<&'static Property> {
        self.resolve(path)?.ok_or(Unsupported("no such property"))
    }
}

fn is_persistent_reference_field(p: &Property) -> bool {
    p.persisted
        && (p.one_to_one
            || p.many_to_one
            || (p.property_type == dhis_schema::PropertyType::Reference && p.identifiable_object))
}

fn is_persistent_collection_field(p: &Property) -> bool {
    p.persisted && p.collection && (p.one_to_many || p.many_to_many)
}

fn is_href_property(p: &Property) -> bool {
    p.key() == "href" && p.klass.as_deref() == Some("java.lang.String")
}

fn is_access_property(p: &Property) -> bool {
    p.key() == "access" && p.klass.as_deref() == Some("org.hisp.dhis.security.acl.Access")
}

/// `GistLogic.isIncludedField`.
fn is_included_field(p: &Property, auto: AutoType) -> bool {
    match p.gist_preferences.included {
        GistInclude::True => true,
        GistInclude::False => false,
        GistInclude::Auto => {
            auto.is_included(p) && p.persisted && p.readable && p.field_name.is_some()
        }
    }
}

/// `GistPlanner.propertyTypeOrder` (a consistent order, so any stable sort agrees with Java's).
fn property_type_order(a: &Property, b: &Property) -> Result<std::cmp::Ordering> {
    let (ta, tb) = if a.collection && b.collection {
        (a.item_property_type.as_ref(), b.item_property_type.as_ref())
    } else {
        (Some(&a.property_type), Some(&b.property_type))
    };
    // A missing item type would be a NullPointerException in Java.
    let (Some(ta), Some(tb)) = (ta, tb) else {
        return Err(Unsupported("collection without item type"));
    };
    let coll = dhis_schema::PropertyType::Collection;
    Ok(match (*ta == coll, *tb == coll) {
        (true, false) => std::cmp::Ordering::Greater,
        (false, true) => std::cmp::Ordering::Less,
        _ => {
            let (Some(oa), Some(ob)) = (ta.ordinal(), tb.ordinal()) else {
                return Err(Unsupported("unknown property type"));
            };
            oa.cmp(&ob)
        }
    })
}

fn field(path: &str, transformation: Transform) -> Result<Field> {
    Ok(Field {
        property_path: PropertyPath::of(path).map_err(|_| Unsupported("invalid property path"))?,
        renamed_path: None,
        transformation,
        args: Vec::new(),
    })
}

const REFS_PATH: &str = "__refs__";

fn refs_field() -> Field {
    Field {
        property_path: PropertyPath::of(REFS_PATH).expect("valid path"),
        renamed_path: Some(PropertyPath::of("apiEndpoints").expect("valid path")),
        transformation: Transform::None,
        args: Vec::new(),
    }
}

fn is_refs(f: &Field) -> bool {
    f.property_path.to_string() == REFS_PATH
}

fn transform_of_preference(p: &Property) -> Transform {
    p.gist_preferences
        .transformation
        .as_deref()
        .map(Transform::of)
        .unwrap_or(Transform::Auto)
}

/// `GistLogic.effectiveTransform` for non-collection properties.
fn effective_transform(p: &Property, fallback: Transform, target: Transform) -> Transform {
    let mut target = if target == Transform::Auto {
        transform_of_preference(p)
    } else {
        target
    };
    if !p.collection {
        return if target == Transform::From {
            Transform::From
        } else {
            Transform::None
        };
    }
    if target == Transform::Auto {
        target = fallback;
    }
    if matches!(target, Transform::Ids | Transform::IdObjects)
        && p.embedded_object
        && is_persistent_collection_field(p)
    {
        return Transform::Size;
    }
    if target == Transform::None {
        return Transform::Size;
    }
    if target == Transform::Auto {
        Transform::None
    } else {
        target
    }
}

/// `GistPlanner#planFields`.
fn plan_fields(ctx: &Ctx, params: &ListParams) -> Result<Vec<Field>> {
    let parsed = Fields::of(&params.fields).map_err(|_| Unsupported("invalid fields"))?;
    let mut fields = parsed.0;
    if fields.is_empty() {
        fields = vec![field(":all", Transform::None)?];
    }

    // withPresetFields
    let explicit: HashSet<String> = fields.iter().map(|f| f.property_path.to_string()).collect();
    let mut expanded: Vec<Field> = Vec::new();
    for f in &fields {
        let path = &f.property_path;
        if path.is_preset() {
            let preset = path.segments().last().map(String::as_str).unwrap_or("");
            let keep = |p: &Property| -> Result<bool> {
                Ok(match preset {
                    ":all" => is_included_field(p, params.auto),
                    ":persisted" => p.persisted,
                    ":owner" => p.persisted && p.owner,
                    // need Java's getter methods; other presets are an error (E2038)
                    _ => return Err(Unsupported("field preset")),
                })
            };
            let mut props: Vec<&Property> = Vec::new();
            for p in &ctx.home.properties {
                if keep(p)? {
                    props.push(p);
                }
            }
            let mut err = None;
            props.sort_by(|a, b| {
                property_type_order(a, b).unwrap_or_else(|e| {
                    err = Some(e);
                    std::cmp::Ordering::Equal
                })
            });
            if let Some(e) = err {
                return Err(e);
            }
            for p in props {
                let key = p.key();
                if explicit.contains(key)
                    || explicit.contains(&format!("-{key}"))
                    || explicit.contains(&format!("!{key}"))
                {
                    continue;
                }
                // canRead(schema, name): superusers read every property
                expanded.push(field(key, Transform::Auto)?);
                // addReferenceFields: references to types without their own endpoint are
                // expanded into nested fields
                if is_persistent_reference_field(p) {
                    let target = p
                        .klass
                        .as_deref()
                        .and_then(registered::by_klass)
                        .ok_or(Unsupported("reference to unregistered type"))?;
                    if target.relative_api_endpoint.is_none() {
                        return Err(Unsupported("embedded reference fields"));
                    }
                }
            }
        } else if path.is_exclude() {
            let excluded = path.property().to_string();
            expanded.retain(|e| e.property_path.to_string() != excluded);
        } else {
            expanded.push(f.clone());
        }
    }
    let mut fields = expanded;

    // withAttributeFields
    for f in &fields {
        if f.transformation == Transform::Auto
            && !f.property_path.is_nested()
            && is_uid_path(&f.property_path)
            && ctx.resolve(&f.property_path)?.is_none()
        {
            return Err(Unsupported("attribute field"));
        }
    }

    // withTranslatedFields
    for f in fields.iter_mut() {
        if f.transformation != Transform::Auto {
            continue;
        }
        let Some(p) = ctx.resolve(&f.property_path)? else {
            continue;
        };
        let name = resolve_translation_base_property_name(&p.name);
        // path.withTail(name): same parent
        let base_path = match f.property_path.parent() {
            Some(parent) => format!("{parent}.{name}"),
            None => name.clone(),
        };
        let base_path = PropertyPath::of(&base_path).map_err(|_| Unsupported("invalid path"))?;
        let Some(b) = ctx.resolve(&base_path)? else {
            continue;
        };
        if !b.translatable || name == p.name {
            continue;
        }
        let renamed = f.path().clone();
        f.property_path = base_path;
        f.renamed_path = Some(renamed);
        f.transformation = Transform::Translate;
        f.args.clear();
    }

    // withCollectionItemPropertyAsTransformation: items of a collection become a transformation
    for f in &fields {
        if let Some(parent) = f.property_path.parent()
            && ctx.resolve_mandatory(&parent)?.collection
        {
            return Err(Unsupported("collection item field"));
        }
    }

    // withEffectiveTransformation
    for f in fields.iter_mut() {
        if f.transformation == Transform::Attribute || f.transformation == Transform::Translate {
            continue;
        }
        let p = ctx.resolve_mandatory(&f.property_path)?;
        let t = effective_transform(p, params.auto.default_transformation(), f.transformation);
        if t != f.transformation {
            f.transformation = t;
            f.args.clear();
        }
    }

    // withEndpointsField
    if params.references {
        let mut has_references = false;
        for f in &fields {
            let p = ctx.resolve_mandatory(&f.property_path)?;
            if (is_persistent_reference_field(p) && p.identifiable_object)
                || is_persistent_collection_field(p)
            {
                has_references = true;
            }
        }
        if has_references {
            fields.push(refs_field());
        }
    }
    Ok(fields)
}

/// `PropertyPath#isUID()`.
fn is_uid_path(path: &PropertyPath) -> bool {
    let s = path.segments().last().map(String::as_str).unwrap_or("");
    s.len() == 11 && dhis_core_model::uid::is_valid_uid(s)
}

/// `GistPlanner#planFilters` and `GistValidator` for filters.
fn plan_filters(ctx: &Ctx, filters: &[Filter]) -> Result<()> {
    if filters.len() > 1 && filters.iter().any(|f| f.group >= 0) {
        return Err(Unsupported("filter groups"));
    }
    for f in filters {
        if f.path.is_nested() {
            return Err(Unsupported("nested filter"));
        }
        if is_uid_path(&f.path) && ctx.resolve(&f.path)?.is_none() {
            return Err(Unsupported("attribute filter"));
        }
        if f.op.is_access_compare() {
            return Err(Unsupported("access filter"));
        }
        let p = ctx.resolve_mandatory(&f.path)?;
        if p.collection {
            return Err(Unsupported("collection filter"));
        }
        // validateFilterPath / validateFilterArgument
        if !p.persisted {
            return Err(Unsupported("filter on non-persisted property"));
        }
        if f.op.is_unary() {
            if !f.values.is_empty() {
                return Err(Unsupported("unary filter with argument"));
            }
        } else if f.values.is_empty() {
            return Err(Unsupported("binary filter without argument"));
        }
        if !f.op.is_multi_value() && f.values.len() > 1 {
            return Err(Unsupported("several arguments"));
        }
    }
    Ok(())
}

/// `GistValidator#validateOrder`.
fn validate_order(ctx: &Ctx, order: &Order) -> Result<()> {
    let p = ctx.resolve_mandatory(&order.path)?;
    if p.simple {
        if p.persisted {
            return Ok(());
        }
        if let Some(base) = ctx.resolve_name(&resolve_translation_base_property_name(&p.name))?
            && base.persisted
            && base.can_be_translated()
        {
            return Ok(());
        }
    }
    Err(Unsupported("invalid order property"))
}

/// `GistValidator#validateField` (superusers pass every access check).
fn validate_field(ctx: &Ctx, f: &Field, access: &Access) -> Result<()> {
    if is_refs(f) {
        return Ok(());
    }
    let p = ctx.resolve_mandatory(&f.property_path)?;
    if f.property_path.is_nested() {
        let path = ctx
            .resolve_path(&f.property_path)?
            .ok_or(Unsupported("no such property"))?;
        if path[0].collection && path[0].persisted {
            return Err(Unsupported("field computes to many values"));
        }
        // validateFieldAccess: the field's owner type must be readable (some User properties are
        // public)
        let owner = ctx.owner_of(&f.property_path)?;
        const PUBLIC: [&str; 3] = ["sharing", "access", "translations"];
        const PUBLIC_USER: [&str; 7] = [
            "id",
            "code",
            "displayName",
            "name",
            "surname",
            "firstName",
            "username",
        ];
        let public = PUBLIC.contains(&p.name.as_str())
            || (owner.klass == "org.hisp.dhis.user.User" && PUBLIC_USER.contains(&p.name.as_str()));
        if owner.identifiable_object && !public && !access.can_read(owner) {
            return Err(Unsupported("field of a type the user cannot read"));
        }
    }
    if matches!(
        f.transformation,
        Transform::Pluck | Transform::Member | Transform::NotMember | Transform::From
    ) {
        return Err(Unsupported("field transformation"));
    }
    if !p.readable {
        return Err(Unsupported("property not readable"));
    }
    Ok(())
}

/// Column (and type) of a simple persisted property.
fn column(entity: &Entity, p: &Property) -> Result<(String, ColumnType)> {
    let field_name = p
        .field_name
        .as_deref()
        .ok_or(Unsupported("property without field"))?;
    let mapped = entity
        .properties
        .get(field_name)
        .ok_or(Unsupported("property not in the mapping"))?;
    if mapped.kind != "property" || mapped.formula {
        return Err(Unsupported("mapped property kind"));
    }
    let col = mapped.column.clone().ok_or(Unsupported("no column"))?;
    // an enum stored by name: a text column, written with Enum#name()
    if p.property_type.name() == "CONSTANT" {
        return match mapped.hibernate_type.as_deref() {
            Some("enum-string") => Ok((col, ColumnType::Text)),
            _ => Err(Unsupported("enum mapping")),
        };
    }
    let ty = match p.klass.as_deref() {
        Some("java.lang.String") => ColumnType::Text,
        Some("java.lang.Double" | "double") => ColumnType::Double,
        Some("java.lang.Integer" | "int") => ColumnType::Int,
        Some("java.lang.Long" | "long") => ColumnType::Long,
        Some("java.lang.Boolean" | "boolean") => ColumnType::Bool,
        Some("java.util.Date") => ColumnType::Timestamp,
        _ => return Err(Unsupported("property type")),
    };
    let type_ok = match mapped.hibernate_type.as_deref() {
        None => true,
        Some("text" | "string") => ty == ColumnType::Text,
        // `date` columns become a java.sql.Date at midnight, printed the same way
        Some("timestamp" | "date") => ty == ColumnType::Timestamp,
        Some("double") => ty == ColumnType::Double,
        Some("integer") => ty == ColumnType::Int,
        Some("long") => ty == ColumnType::Long,
        Some("boolean") => ty == ColumnType::Bool,
        _ => false,
    };
    if !type_ok {
        return Err(Unsupported("hibernate type"));
    }
    Ok((col, ty))
}

fn output_type(ty: ColumnType) -> OutputType {
    match ty {
        ColumnType::Text => OutputType::String,
        ColumnType::Double => OutputType::Double,
        ColumnType::Int | ColumnType::Long => OutputType::Integer,
        ColumnType::Bool => OutputType::Boolean,
        ColumnType::Timestamp => OutputType::Date,
        ColumnType::TextArray => OutputType::StringArray,
        ColumnType::Json(_) => OutputType::Raw,
    }
}

/// SQL to select a column of type `ty`.
fn select_expr(expr: &str, ty: ColumnType) -> String {
    match ty {
        ColumnType::Timestamp => format!("to_char({expr}, 'YYYY-MM-DD HH24:MI:SS.US')"),
        _ => expr.to_string(),
    }
}

/// Validates a locale or translation key before it is embedded in SQL like Java embeds it in
/// HQL (Java does not check; anything unusual goes to Java).
fn sql_literal_safe(s: &str) -> Result<&str> {
    if !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        Ok(s)
    } else {
        Err(Unsupported("unusual locale or translation key"))
    }
}

fn translated_expr(entity: &Entity, p: &Property, locale: &str) -> Result<String> {
    let key = p
        .translation_key
        .as_deref()
        .ok_or(Unsupported("no translation key"))?;
    let translations = entity
        .properties
        .get("translations")
        .filter(|m| m.kind == "property")
        .and_then(|m| m.column.clone())
        .ok_or(Unsupported("no translations column"))?;
    let (col, ty) = column(entity, p)?;
    if ty != ColumnType::Text {
        return Err(Unsupported("translated non-text"));
    }
    Ok(format!(
        "coalesce(jsonb_get_translated_value(e.{translations}, '{}', '{}'), e.{col})",
        sql_literal_safe(key)?,
        sql_literal_safe(locale)?
    ))
}

/// A filter argument as `DefaultGistService#parseFilterArgument` reads it (Jackson): only the
/// forms every Jackson setting agrees on are taken.
fn parse_argument(value: &str, ty: ColumnType) -> Result<SqlParam> {
    let json_number = |s: &str, integral: bool| {
        let b = s.as_bytes();
        let mut i = 0;
        if b.first() == Some(&b'-') {
            i += 1;
        }
        let digits_start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        let int_digits = i - digits_start;
        if int_digits == 0 || (int_digits > 1 && b[digits_start] == b'0') {
            return false;
        }
        if integral {
            return i == b.len();
        }
        if i < b.len() && b[i] == b'.' {
            i += 1;
            let f = i;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
            if i == f {
                return false;
            }
        }
        if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
            i += 1;
            if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
                i += 1;
            }
            let e = i;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
            if i == e {
                return false;
            }
        }
        i == b.len()
    };
    let bad = Unsupported("filter argument");
    Ok(match ty {
        ColumnType::Text => SqlParam::Text(value.to_string()),
        ColumnType::Double if json_number(value, false) => {
            let d: f64 = value.parse().map_err(|_| bad)?;
            if !d.is_finite() {
                return Err(bad);
            }
            SqlParam::Double(d)
        }
        ColumnType::Int if json_number(value, true) => {
            SqlParam::Int(value.parse().map_err(|_| bad)?)
        }
        ColumnType::Long if json_number(value, true) => {
            SqlParam::Long(value.parse().map_err(|_| bad)?)
        }
        ColumnType::Bool if value == "true" || value == "false" => SqlParam::Bool(value == "true"),
        _ => return Err(bad),
    })
}

/// `GistBuilder#createFilterHQL` for a non-nested simple persisted property.
fn filter_sql(
    ctx: &Ctx,
    entity: &Entity,
    f: &Filter,
    params: &mut Vec<SqlParam>,
) -> Result<String> {
    let p = ctx.resolve_mandatory(&f.path)?;
    if p.property_type.name() == "PASSWORD" {
        return Err(Unsupported("password filter"));
    }
    if !p.simple {
        return Err(Unsupported("filter on non-simple property"));
    }
    if p.property_type.name() == "CONSTANT" {
        // Jackson rejects names that are no constant of the enum; the constants are not known here
        return Err(Unsupported("enum filter"));
    }
    let (col, ty) = column(entity, p)?;
    let op = f.op;
    let string_length = ty == ColumnType::Text
        && p.property_type == dhis_schema::PropertyType::Text
        && (op.is_emptiness_compare()
            || (op.is_order_compare()
                && f.values.len() == 1
                && (1..=5).contains(&f.values[0].len())
                && f.values[0].bytes().all(|b| b.is_ascii_digit())));
    let left = if string_length {
        format!("length(e.{col})")
    } else if op.is_emptiness_compare() {
        // Java compares the column itself with 0
        return Err(Unsupported("emptiness filter on non-text"));
    } else if op.is_case_insensitive() {
        if ty != ColumnType::Text {
            return Err(Unsupported("case-insensitive filter on non-text"));
        }
        format!("lower(e.{col})")
    } else {
        format!("e.{col}")
    };
    if op.is_unary() {
        return Ok(format!("{left} {}", op.sql()));
    }
    if op.is_string_compare() && ty != ColumnType::Text {
        // Java casts the parsed argument to String: ClassCastException
        return Err(Unsupported("string filter on non-text"));
    }
    let mut placeholders = Vec::new();
    for v in &f.values {
        let param = if string_length {
            SqlParam::Int(v.parse().map_err(|_| Unsupported("filter argument"))?)
        } else if op.is_string_compare() {
            if op.is_case_insensitive() && !v.is_ascii() {
                // Java lower-cases with the JVM locale before SQL lower(): only ASCII is certain
                return Err(Unsupported("non-ASCII case-insensitive pattern"));
            }
            let v = if op.is_case_insensitive() {
                v.to_ascii_lowercase()
            } else {
                v.clone()
            };
            SqlParam::Text(complete_like_expression(op, &v))
        } else {
            parse_argument(v, ty)?
        };
        params.push(param);
        let ph = format!("${}", params.len());
        placeholders.push(if op.is_case_insensitive() {
            format!("lower({ph})")
        } else {
            ph
        });
    }
    let right = if op.is_multi_value() {
        format!("({})", placeholders.join(", "))
    } else {
        placeholders.join(", ")
    };
    Ok(format!("{left} {} {right}", op.sql()))
}

/// `GistBuilder.completeLikeExpression`.
fn complete_like_expression(op: Comparison, value: &str) -> String {
    use Comparison::*;
    match op {
        Like | Ilike | NotIlike | NotLike => {
            if value.contains('*') || value.contains('?') {
                value.replace('*', "%").replace('?', "_")
            } else {
                format!("%{value}%")
            }
        }
        StartsLike | StartsWith | NotStartsLike | NotStartsWith => format!("{value}%"),
        EndsLike | EndsWith | NotEndsLike | NotEndsWith => format!("%{value}"),
        _ => value.to_string(),
    }
}

/// `GistBuilder#createOrderByHQL`.
fn order_sql(ctx: &Ctx, entity: &Entity, orders: &[Order], locale: &str) -> Result<String> {
    if orders.is_empty() {
        // properties with a gist order, by that order (stable): `<fieldName> asc`, which
        // Hibernate resolves on the root entity (a reference sorts by its foreign key)
        let mut by_order: Vec<&Property> = ctx
            .home
            .properties
            .iter()
            .filter(|p| p.gist_preferences.order.is_some())
            .collect();
        if !by_order.is_empty() {
            by_order.sort_by_key(|p| p.gist_preferences.order);
            let mut out = Vec::new();
            for p in by_order {
                let field = p.field_name.as_deref().ok_or(Unsupported("no field"))?;
                let m = entity
                    .properties
                    .get(field)
                    .ok_or(Unsupported("order property not mapped"))?;
                let col = match m.kind.as_str() {
                    "property" | "many-to-one" if !m.formula && m.column_table.is_none() => {
                        m.column.as_deref().ok_or(Unsupported("no column"))?
                    }
                    _ => return Err(Unsupported("gist default order mapping")),
                };
                out.push(format!("e.{col} asc"));
            }
            return Ok(out.join(", "));
        }
        let id = entity.id.as_ref().ok_or(Unsupported("no id"))?;
        return Ok(format!("e.{} asc", id.column));
    }
    let mut out = Vec::new();
    for o in orders {
        let dir = if o.desc { "desc" } else { "asc" };
        let base = resolve_translation_base_property_name(&o.path.to_string());
        match ctx.resolve_name(&base)? {
            Some(p) if p.can_be_translated() => {
                out.push(format!("{} {dir}", translated_expr(entity, p, locale)?));
            }
            _ => {
                let p = ctx.resolve_mandatory(&o.path)?;
                let (col, _) = column(entity, p)?;
                out.push(format!(" e.{col} {dir}"));
            }
        }
    }
    Ok(out.join(","))
}

/// Plans a list query (`GistPipeline#createListQuery` → `DefaultGistService#exportObjectList`).
pub fn plan_list(
    schema: &'static RegisteredSchema,
    params: &ListParams,
    access: Access,
) -> Result<ListPlan> {
    if !access.superuser && !access.can_read_type {
        // Java denies the fields (403) or, in presets, leaves most of them out
        return Err(Unsupported("type not readable"));
    }
    let entity = hibernate::entity(&schema.klass).ok_or(Unsupported("not hbm-mapped"))?;
    if !entity.is_plain_class() || schema.klass == "org.hisp.dhis.attribute.Attribute" {
        return Err(Unsupported("mapping shape"));
    }
    let table = entity.table.as_deref().ok_or(Unsupported("no table"))?;
    let ctx = Ctx { home: schema };

    // paging: abs(page), min(1000, abs(pageSize)); pageSize 0 divides by zero in Java
    let page = params.page.checked_abs().ok_or(Unsupported("page"))?;
    let size = params
        .page_size
        .checked_abs()
        .ok_or(Unsupported("pageSize"))?
        .min(1000);
    if size == 0 {
        return Err(Unsupported("pageSize 0"));
    }
    let offset = (page.max(1) - 1)
        .checked_mul(size)
        .ok_or(Unsupported("page offset"))?;

    let filters = Filter::of_list(&params.filter)?;
    let orders = Order::of_list(&params.order)?;
    let planned = plan_fields(&ctx, params)?;
    plan_filters(&ctx, &filters)?;
    for o in &orders {
        validate_order(&ctx, o)?;
    }
    for f in &planned {
        validate_field(&ctx, f, &access)?;
    }

    // addSupportFields
    let mut fetch = planned.clone();
    for f in &planned {
        if is_refs(f) {
            continue;
        }
        let p = ctx.resolve_mandatory(&f.property_path)?;
        if is_access_property(p) {
            return Err(Unsupported("access field"));
        }
        // the same-parent id (`path.withTail("id")`)
        let id_path = match f.property_path.parent() {
            Some(parent) => format!("{parent}.id"),
            None => "id".to_string(),
        };
        if (is_persistent_collection_field(p) || is_href_property(p))
            && !fetch.iter().any(|g| g.property_path.to_string() == id_path)
        {
            fetch.push(field(&id_path, Transform::Auto)?);
        }
    }

    // createFieldsHQL: index by property path, the last one wins
    let index: HashMap<String, usize> = fetch
        .iter()
        .enumerate()
        .map(|(i, f)| (f.property_path.to_string(), i))
        .collect();
    let id_slot = index.get("id").copied();
    let endpoint_root = schema.relative_api_endpoint.as_ref().map(|rel| {
        let root = if params.absolute_urls {
            format!("{}/api", params.context_root)
        } else {
            "/api".to_string()
        };
        format!("{root}{rel}")
    });
    let url_params = if params.absolute_urls {
        "?absoluteUrls=true"
    } else {
        ""
    };

    let mut selects: Vec<String> = Vec::new();
    let mut slots: Vec<Slot> = Vec::new();
    let mut ref_entries: Vec<RefEntry> = Vec::new();
    let mut joins = Joins::default();
    let mut output: Vec<OutputProperty> = Vec::new();
    for (i, f) in fetch.iter().enumerate() {
        if is_refs(f) {
            slots.push(Slot::Refs { entries: vec![] });
            output.push(OutputProperty {
                path: f.path().to_string(),
                ty: OutputType::StringMap,
            });
            continue;
        }
        let p = ctx.resolve_mandatory(&f.property_path)?;
        let out_path = f.path().to_string();
        if let Some(parent) = f.property_path.parent() {
            // a field of a referenced object (`ref.field`): the reference is joined
            let head = ctx.resolve_mandatory(&parent)?;
            if !is_persistent_reference_field(head) || head.collection {
                return Err(Unsupported("nested field of this kind"));
            }
            let target = reference_target(entity, head)?;
            let alias = joins.alias(head.key(), &target, i);
            let (expr, ty, out) =
                if f.transformation == Transform::Translate && p.can_be_translated() {
                    // GistBuilder#createTranslatedFieldHQL always uses the root's translations and
                    // the root's property of that name (`e.`), whatever the path: reproduced
                    (
                        translated_expr(entity, p, &params.translation_locale)?,
                        ColumnType::Text,
                        OutputType::String,
                    )
                } else if is_href_property(p) {
                    let id_slot = index.get(&format!("{parent}.id")).copied();
                    slots.push(Slot::Href {
                        id_slot,
                        prefix: target.schema.relative_api_endpoint.as_ref().map(|rel| {
                            let root = if params.absolute_urls {
                                format!("{}/api", params.context_root)
                            } else {
                                "/api".to_string()
                            };
                            format!("{root}{rel}/")
                        }),
                        suffix: format!("/gist{url_params}"),
                    });
                    output.push(OutputProperty {
                        path: out_path,
                        ty: OutputType::String,
                    });
                    continue;
                } else if p.collection
                    || is_persistent_reference_field(p)
                    || json_column(target.entity, p).is_some()
                    || f.transformation == Transform::From
                {
                    return Err(Unsupported("nested field of this kind"));
                } else {
                    let (col, ty) = column(target.entity, p)?;
                    (
                        select_expr(&format!("{alias}.{col}"), ty),
                        ty,
                        output_type(ty),
                    )
                };
            selects.push(expr);
            slots.push(Slot::Sql(ty));
            output.push(OutputProperty {
                path: out_path,
                ty: out,
            });
            continue;
        }
        if f.transformation == Transform::Translate && p.can_be_translated() {
            selects.push(translated_expr(entity, p, &params.translation_locale)?);
            slots.push(Slot::Sql(ColumnType::Text));
            output.push(OutputProperty {
                path: out_path,
                ty: OutputType::String,
            });
        } else if is_href_property(p) {
            slots.push(Slot::Href {
                id_slot,
                prefix: endpoint_root.as_ref().map(|r| format!("{r}/")),
                suffix: format!("/gist{url_params}"),
            });
            output.push(OutputProperty {
                path: out_path,
                ty: OutputType::String,
            });
        } else if f.transformation == Transform::From {
            return Err(Unsupported("from transformation"));
        } else if is_persistent_reference_field(p) {
            selects.push(reference_expr(entity, p, i, &mut joins)?);
            slots.push(Slot::Sql(ColumnType::Text));
            if p.identifiable_object
                && params.references
                && let (Some(root), Some(id)) = (&endpoint_root, id_slot)
            {
                ref_entries.push(RefEntry {
                    key: f.path().to_string(),
                    id_slot: id,
                    prefix: format!("{root}/"),
                    suffix: format!("/{}/gist{url_params}", p.key()),
                });
            }
            output.push(OutputProperty {
                path: out_path,
                ty: OutputType::String,
            });
        } else if is_persistent_collection_field(p) {
            // createCollectionFieldHQL: apiEndpoints entry, then the transformation
            if params.references
                && let (Some(root), Some(id)) = (&endpoint_root, id_slot)
            {
                ref_entries.push(RefEntry {
                    key: f.path().to_string(),
                    id_slot: id,
                    prefix: format!("{root}/"),
                    suffix: format!("/{}/gist{url_params}", p.key()),
                });
            }
            let join = collection_join(entity, p)?;
            let owner_id = &entity.id.as_ref().ok_or(Unsupported("no id"))?.column;
            let (expr, ty, out) =
                collection_expr(&join, p, f.transformation, &access, owner_id, i)?;
            selects.push(expr);
            slots.push(Slot::Sql(ty));
            output.push(OutputProperty {
                path: out_path,
                ty: out,
            });
        } else if let Some((col, kind)) = json_column(entity, p) {
            selects.push(format!("e.{col}::text"));
            slots.push(Slot::Sql(ColumnType::Json(kind)));
            output.push(OutputProperty {
                path: out_path,
                ty: OutputType::Raw,
            });
        } else if p.collection && p.owning_role.is_some() {
            // other mapped collections (element collections): `size(e.path)`, an Integer
            let join = collection_join(entity, p)?;
            let owner_id = &entity.id.as_ref().ok_or(Unsupported("no id"))?.column;
            let (expr, ty, out) = collection_expr(&join, p, Transform::Size, &access, owner_id, i)?;
            selects.push(expr);
            slots.push(Slot::Sql(ty));
            output.push(OutputProperty {
                path: out_path,
                ty: out,
            });
        } else if p.collection {
            return Err(Unsupported("collection field"));
        } else {
            let (col, ty) = column(entity, p)?;
            selects.push(select_expr(&format!("e.{col}"), ty));
            slots.push(Slot::Sql(ty));
            output.push(OutputProperty {
                path: out_path,
                ty: output_type(ty),
            });
        }
    }
    // only the planned fields are rendered; support fields are fetched only
    output.truncate(planned.len());
    if !ref_entries.is_empty() {
        let refs_slot = index
            .get(REFS_PATH)
            .copied()
            .ok_or(Unsupported("refs without slot"))?;
        // TreeMap order (non-nested paths compare as strings)
        ref_entries.sort_by(|a, b| a.key.encode_utf16().cmp(b.key.encode_utf16()));
        ref_entries.dedup_by(|a, b| a.key == b.key);
        slots[refs_slot] = Slot::Refs {
            entries: ref_entries,
        };
    }

    let mut sql_params = Vec::new();
    let junction = if params.any_filter { " or " } else { " and " };
    let filter_sql = if filters.is_empty() {
        "1=1".to_string()
    } else {
        let mut parts = Vec::new();
        for f in &filters {
            parts.push(filter_sql(&ctx, entity, f, &mut sql_params)?);
        }
        parts.join(junction)
    };
    // createAccessFilterHQL: the sharing filter unless superuser or no persisted sharing
    let access_sql = match ctx.home.property("sharing") {
        Some(p) if p.persisted && !access.superuser => sharing_filter(entity, &access)?,
        _ => "1=1".to_string(),
    };
    let order = order_sql(&ctx, entity, &orders, &params.translation_locale)?;
    if selects.is_empty() {
        // only computed fields (cannot happen: href brings its id)
        return Err(Unsupported("no selected column"));
    }
    let select_list = selects.join(", ");
    let join_from: String = joins
        .entries
        .iter()
        .map(|(_, _, j, _)| j.as_str())
        .collect();
    let join_where: String = joins
        .entries
        .iter()
        .map(|(_, _, _, c)| format!("{c} and "))
        .collect();
    let fetch_sql = format!(
        "select {select_list} from {table} e{join_from} where {join_where}({filter_sql}) and ({access_sql}) order by {order} limit {} offset {offset}",
        size.max(1)
    );
    let count_sql = params
        .total
        .then(|| format!("select count(*) from {table} e where ({filter_sql}) and ({access_sql})"));
    Ok(ListPlan {
        schema,
        fetch_sql,
        count_sql,
        params: sql_params,
        slots,
        output,
        page: 1 + offset / size,
        page_size: size,
    })
}

/// A JSONB column of one of the supported DHIS2 JSON types.
fn json_column(entity: &Entity, p: &Property) -> Option<(String, JsonKind)> {
    let m = entity.properties.get(p.field_name.as_deref()?)?;
    if m.kind != "property" || m.formula || m.column_table.is_some() {
        return None;
    }
    let kind = match m.hibernate_type.as_deref()? {
        "jsbObjectSharing" => JsonKind::Sharing,
        "jblTranslations" => JsonKind::Translations {
            wrapped: m.embedded,
        },
        "jsbAttributeValues" => JsonKind::AttributeValues,
        _ => return None,
    };
    Some((m.column.clone()?, kind))
}

/// How a persistent collection is stored.
enum Join<'m> {
    /// A join table (many-to-many, both sides): `key` points to the owner, `elem` to the item.
    Table {
        table: &'m str,
        key: &'m str,
        elem: &'m str,
        item: &'m str,
    },
    /// Rows of the item table with a foreign key to the owner (one-to-many).
    Child { table: &'m str, fk: &'m str },
    /// An element collection table (values, not entities).
    Elements { table: &'m str, key: &'m str },
}

/// The mapping of a persistent collection property of `entity`.
fn collection_join<'m>(entity: &'m Entity, p: &Property) -> Result<Join<'m>> {
    let field = p.field_name.as_deref().ok_or(Unsupported("no field"))?;
    let m = entity
        .properties
        .get(field)
        .ok_or(Unsupported("collection not in the mapping"))?;
    let unsupported = Unsupported("collection mapping");
    match m.kind.as_str() {
        "set" | "list" | "bag" | "idbag" => {}
        "element-collection" => {
            return Ok(Join::Elements {
                table: m.table.as_deref().ok_or(unsupported)?,
                key: m.key_column.as_deref().ok_or(unsupported)?,
            });
        }
        _ => return Err(unsupported),
    }
    let item = m.element_class.as_deref().ok_or(unsupported)?;
    if let (Some(table), Some(key), Some(elem)) = (
        m.table.as_deref(),
        m.key_column.as_deref(),
        m.element_column.as_deref(),
    ) {
        return Ok(Join::Table {
            table,
            key,
            elem,
            item,
        });
    }
    let item_entity = hibernate::entity(item).ok_or(unsupported)?;
    if !item_entity.is_plain_class() {
        return Err(unsupported);
    }
    let item_table = item_entity.table.as_deref().ok_or(unsupported)?;
    // JPA mappedBy: the owning side on the item class
    if let Some(owner_prop) = m.mapped_by.as_deref() {
        let o = item_entity.properties.get(owner_prop).ok_or(unsupported)?;
        return match (o.kind.as_str(), &o.table, &o.key_column, &o.element_column) {
            ("set" | "list" | "bag", Some(t), Some(k), Some(el)) => Ok(Join::Table {
                table: t,
                key: el,
                elem: k,
                item,
            }),
            ("many-to-one", _, _, _) if o.column_table.is_none() => Ok(Join::Child {
                table: item_table,
                fk: o.column.as_deref().ok_or(unsupported)?,
            }),
            _ => Err(unsupported),
        };
    }
    // hbm one-to-many: the key column is on the item table
    if m.table.is_none() && m.element_column.is_none() {
        return Ok(Join::Child {
            table: item_table,
            fk: m.key_column.as_deref().ok_or(unsupported)?,
        });
    }
    Err(unsupported)
}

/// `createCollectionFieldHQL` for the supported transformations, as Hibernate 5 renders them
/// (`size()`: a count of the collection's rows; `elements()`: `item.id in (join.elem)` over a
/// cross join).
fn collection_expr(
    join: &Join<'_>,
    p: &Property,
    t: Transform,
    access: &Access,
    owner_id: &str,
    index: usize,
) -> Result<(String, ColumnType, OutputType)> {
    let item_entity = match join {
        Join::Table { item, .. } => hibernate::entity(item).filter(|e| e.is_plain_class()),
        _ => None,
    };
    let item_schema = p.item_klass.as_deref().and_then(registered::by_klass);
    let filter_by_sharing = !access.superuser
        && item_schema
            .and_then(|s| s.property("sharing"))
            .is_some_and(|s| s.persisted);
    let a = format!("i{index}");
    let j = format!("j{index}");
    match t {
        Transform::Size if !filter_by_sharing => {
            let (table, key) = match join {
                Join::Table { table, key, .. } | Join::Elements { table, key } => (table, key),
                Join::Child { table, fk } => (table, fk),
            };
            Ok((
                format!(
                    "(select count({j}.{key}) from {table} {j} where e.{owner_id} = {j}.{key})"
                ),
                ColumnType::Long,
                OutputType::Integer,
            ))
        }
        Transform::Size | Transform::Ids | Transform::IdObjects => {
            // `elements()`: the item rows whose id is in the collection's join rows; for a
            // one-to-many those join rows are the item table itself
            let child_entity;
            let (table, key, elem, item_entity) = match join {
                Join::Table {
                    table, key, elem, ..
                } => (
                    *table,
                    *key,
                    *elem,
                    item_entity.ok_or(Unsupported("item mapping"))?,
                ),
                Join::Child { table, fk } => {
                    child_entity = p
                        .item_klass
                        .as_deref()
                        .and_then(hibernate::entity)
                        .filter(|e| e.is_plain_class())
                        .ok_or(Unsupported("item mapping"))?;
                    let id = &child_entity
                        .id
                        .as_ref()
                        .ok_or(Unsupported("item id"))?
                        .column;
                    (*table, *fk, id.as_str(), child_entity)
                }
                Join::Elements { .. } => {
                    return Err(Unsupported("collection transformation on this mapping"));
                }
            };
            let item_table = item_entity
                .table
                .as_deref()
                .ok_or(Unsupported("item table"))?;
            let item_id = &item_entity
                .id
                .as_ref()
                .ok_or(Unsupported("item id"))?
                .column;
            let access_sql = if filter_by_sharing {
                sharing_filter_on(item_entity, access, &a)?
            } else {
                "1=1".to_string()
            };
            let from = format!(
                "from {item_table} {a} cross join {table} {j} where e.{owner_id}={j}.{key} and ({a}.{item_id} in ({j}.{elem})) and {access_sql}"
            );
            if t == Transform::Size {
                return Ok((
                    format!("(select count(*) {from})"),
                    ColumnType::Long,
                    OutputType::Integer,
                ));
            }
            // determineReferenceProperty: the item's id property (uid)
            let item_schema = item_schema.ok_or(Unsupported("item schema"))?;
            let id = item_schema
                .property("id")
                .filter(|p| p.persisted)
                .ok_or(Unsupported("item without id"))?;
            if item_schema.klass.ends_with(".Period") || item_schema.klass.ends_with(".PeriodType")
            {
                return Err(Unsupported("period items"));
            }
            let (uid, _) = column(item_entity, id)?;
            Ok((
                format!("(select array_agg({a}.{uid}) {from})"),
                ColumnType::TextArray,
                if t == Transform::Ids {
                    OutputType::StringArray
                } else {
                    OutputType::IdObjects
                },
            ))
        }
        _ => Err(Unsupported("collection transformation")),
    }
}

/// `JpaQueryUtils.generateHqlQueryForSharingCheck(alias, user, "r%")` on `e` (HQL's
/// `jsonb_extract_path_text` for `->>`).
fn sharing_filter(entity: &Entity, access: &Access) -> Result<String> {
    sharing_filter_on(entity, access, "e")
}

/// The read-sharing condition (`JpaQueryUtils.generateSQlQueryForSharingCheck`, `r%`) for a user
/// on the rows of `entity` aliased `alias`; `None` for values not safe to embed.
pub fn sharing_condition(
    alias: &str,
    entity: &Entity,
    uid: &str,
    group_ids: &[String],
) -> Option<String> {
    let access = Access {
        superuser: false,
        skip_sharing_check: false,
        can_read_type: true,
        user: Some((uid.to_string(), group_ids.to_vec())),
        authorities: Vec::new(),
    };
    sharing_filter_on(entity, &access, alias).ok()
}

/// The sharing filter on the rows of `entity` aliased `alias`.
fn sharing_filter_on(entity: &Entity, access: &Access, alias: &str) -> Result<String> {
    if access.skip_sharing_check {
        return Ok("1=1".into());
    }
    let (uid, groups) = access
        .user
        .as_ref()
        .ok_or(Unsupported("session identity unknown"))?;
    let column = entity
        .properties
        .get("sharing")
        .filter(|m| m.kind == "property")
        .and_then(|m| m.column.as_deref())
        .ok_or(Unsupported("no sharing column"))?;
    let s = format!("{alias}.{column}");
    let uid = sql_literal_safe(uid)?;
    for g in groups {
        sql_literal_safe(g)?;
    }
    let access_pattern = "r%";
    let mut sql = format!(
        " ( jsonb_extract_path_text({s}, 'owner') is null or jsonb_extract_path_text({s}, 'owner') = '{uid}')  \
         or jsonb_extract_path_text({s}, 'public') like '{access_pattern}' or jsonb_extract_path_text({s}, 'public') is null  \
         or (jsonb_has_user_id( {s}, '{uid}') = true  and jsonb_check_user_access( {s}, '{uid}', '{access_pattern}' ) = true )  "
    );
    if !groups.is_empty() {
        let ids = format!("{{{}}}", groups.join(","));
        sql.push_str(&format!(
            " or ( jsonb_has_user_group_ids( {s}, '{ids}') = true  and jsonb_check_user_groups_access( {s}, '{access_pattern}', '{ids}') = true )"
        ));
    }
    Ok(format!("({sql})"))
}

/// `GistBuilder#createReferenceFieldHQL` for a non-required reference to an identifiable type
/// with its own endpoint: `(select R.uid from Target R where R = e.ref)`.
/// The mapping behind a persistent reference to an identifiable type with its own endpoint.
struct RefTarget {
    schema: &'static RegisteredSchema,
    entity: &'static Entity,
    /// Foreign-key column on the owner.
    fk: &'static str,
    table: &'static str,
    id: &'static str,
}

fn reference_target(owner: &'static Entity, p: &Property) -> Result<RefTarget> {
    let target_klass = p.klass.as_deref().ok_or(Unsupported("no class"))?;
    if target_klass.ends_with(".PeriodType") || target_klass.ends_with(".Period") {
        return Err(Unsupported("period reference"));
    }
    let schema = registered::by_klass(target_klass).ok_or(Unsupported("unregistered target"))?;
    let entity = hibernate::entity(target_klass).ok_or(Unsupported("target not mapped"))?;
    if !entity.is_plain_class() {
        return Err(Unsupported("target mapping shape"));
    }
    let field_name = p.field_name.as_deref().ok_or(Unsupported("no field"))?;
    let mapped = owner
        .properties
        .get(field_name)
        .ok_or(Unsupported("reference not in the mapping"))?;
    if mapped.kind != "many-to-one"
        || mapped.class.as_deref() != Some(target_klass)
        || mapped.column_table.is_some()
    {
        return Err(Unsupported("reference mapping"));
    }
    Ok(RefTarget {
        schema,
        entity,
        fk: mapped
            .column
            .as_deref()
            .ok_or(Unsupported("no fk column"))?,
        table: entity.table.as_deref().ok_or(Unsupported("no table"))?,
        id: &entity.id.as_ref().ok_or(Unsupported("no id"))?.column,
    })
}

/// The inner joins of the fetch query: one per referenced property, shared by every field
/// that navigates it (Hibernate 5 renders implicit joins as `cross join` plus a condition).
#[derive(Default)]
struct Joins {
    /// (property key, alias, from clause, condition)
    entries: Vec<(String, String, String, String)>,
}

impl Joins {
    fn alias(&mut self, key: &str, target: &RefTarget, index: usize) -> String {
        if let Some((_, a, _, _)) = self.entries.iter().find(|(k, ..)| k == key) {
            return a.clone();
        }
        let alias = format!("r{index}");
        self.entries.push((
            key.to_string(),
            alias.clone(),
            format!(" cross join {} {alias}", target.table),
            format!("e.{}={alias}.{}", target.fk, target.id),
        ));
        alias
    }
}

/// `GistBuilder#createReferenceFieldHQL` for a reference to an identifiable type with its own
/// endpoint: its uid; `(select R.uid from Target R where R = e.ref)`, or for required ones
/// `e.ref.uid` (an inner join).
fn reference_expr(
    entity: &'static Entity,
    p: &Property,
    index: usize,
    joins: &mut Joins,
) -> Result<String> {
    let t = reference_target(entity, p)?;
    if t.schema.relative_api_endpoint.is_none() {
        return Err(Unsupported("embedded reference"));
    }
    let id = t
        .schema
        .property("id")
        .filter(|p| p.persisted)
        .ok_or(Unsupported("target without id"))?;
    let (uid_col, _) = column(t.entity, id)?;
    if p.required {
        // rows without the reference drop out of the page (not of the count, which has no join)
        let alias = joins.alias(p.key(), &t, index);
        return Ok(format!("{alias}.{uid_col}"));
    }
    let alias = format!("s{index}");
    Ok(format!(
        "(select {alias}.{uid_col} from {table} {alias} where {alias}.{id} = e.{fk})",
        table = t.table,
        id = t.id,
        fk = t.fk
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(fields: &str) -> ListParams {
        ListParams {
            fields: fields.into(),
            filter: String::new(),
            order: String::new(),
            page: 1,
            page_size: 50,
            total: false,
            absolute_urls: false,
            references: true,
            any_filter: false,
            auto: AutoType::S,
            translation_locale: "en".into(),
            context_root: "http://localhost:8081".into(),
        }
    }

    fn constant() -> &'static RegisteredSchema {
        registered::by_singular("constant").unwrap()
    }

    fn sup() -> Access {
        Access::superuser()
    }

    #[test]
    fn default_bridge_fields() {
        let p = plan_list(constant(), &params("id,displayName"), sup()).unwrap();
        assert_eq!(
            p.fetch_sql,
            "select e.uid, coalesce(jsonb_get_translated_value(e.translations, 'NAME', 'en'), e.name) from constant e where (1=1) and (1=1) order by e.constantid asc limit 50 offset 0"
        );
        let names: Vec<&str> = p.output.iter().map(|o| o.path.as_str()).collect();
        assert_eq!(names, ["id", "displayName"]);
    }

    #[test]
    fn all_fields_of_constant() {
        let p = plan_list(constant(), &params("*"), sup()).unwrap();
        let mut names: Vec<&str> = p.output.iter().map(|o| o.path.as_str()).collect();
        // apiEndpoints comes last; the rest follows Java's HashMap order within a type
        assert_eq!(names.pop(), Some("apiEndpoints"));
        names.sort();
        assert_eq!(
            names,
            [
                "code",
                "created",
                "description",
                "id",
                "lastUpdated",
                "lastUpdatedBy",
                "name",
                "shortName",
                "value"
            ]
        );
        assert!(
            p.fetch_sql
                .contains("(select u8.uid from userinfo u8 where u8.userinfoid = e.lastupdatedby)")
                || p.fetch_sql.contains("from userinfo")
        );
    }

    #[test]
    fn filters_and_orders() {
        let mut q = params("id,value");
        q.filter = "value:gt:30,name:ilike:Corpus*,code:!null,name:gt:5".into();
        q.order = "value:desc,displayName".into();
        q.page = 2;
        q.page_size = 3;
        q.total = true;
        let p = plan_list(constant(), &q, sup()).unwrap();
        assert!(p.fetch_sql.contains(
            "where (e.value > $1 and lower(e.name) like lower($2) and e.code is not null and length(e.name) > $3)"
        ), "{}", p.fetch_sql);
        assert!(p.fetch_sql.ends_with(
            "order by  e.value desc,coalesce(jsonb_get_translated_value(e.translations, 'NAME', 'en'), e.name) asc limit 3 offset 3"
        ), "{}", p.fetch_sql);
        assert_eq!(
            p.params,
            [
                SqlParam::Double(30.0),
                SqlParam::Text("corpus%".into()),
                SqlParam::Int(5)
            ]
        );
        assert_eq!(p.page, 2);
        assert!(
            p.count_sql
                .unwrap()
                .starts_with("select count(*) from constant e where (e.value")
        );
    }

    #[test]
    fn unsupported_requests() {
        let un = |fields: &str, filter: &str| {
            let mut q = params(fields);
            q.filter = filter.into();
            plan_list(constant(), &q, sup()).err()
        };
        assert!(un("sharing", "").is_none());
        assert!(un("translations", "").is_none());
        assert!(un("access", "").is_some());
        assert!(un("lastUpdatedBy.name", "").is_none());
        assert!(un("lastUpdatedBy.lastUpdatedBy", "").is_some());
        let p = plan_list(
            constant(),
            &params("id,lastUpdatedBy.username,lastUpdatedBy[id,name]"),
            sup(),
        )
        .unwrap();
        assert!(p.fetch_sql.contains(
            "select e.uid, r1.username, r1.uid, r1.name from constant e cross join userinfo r1 where e.lastupdatedby=r1.userinfoid and (1=1)"
        ), "{}", p.fetch_sql);
        assert!(un("nope", "").is_some());
        assert!(un("id", "value:like:3").is_some());
        assert!(un("id", "lastUpdatedBy:eq:x").is_some());
        assert!(un(":identifiable", "").is_some());
        assert!(un("id", "name:eq:a,1:code:eq:b").is_some());
        assert!(un("id", "name:canread").is_some());
        assert!(un("id", "value:eq:1.").is_some());
        assert!(un("id", "name:ilike:é").is_some());
        let mut q = params("id");
        q.page_size = 0;
        assert!(plan_list(constant(), &q, sup()).is_err());
        let user = |id: Option<(String, Vec<String>)>, readable| Access {
            superuser: false,
            skip_sharing_check: false,
            can_read_type: readable,
            user: id,
            authorities: vec![],
        };
        assert!(plan_list(constant(), &params("id"), user(None, true)).is_err());
        assert!(
            plan_list(
                constant(),
                &params("id"),
                user(Some(("Crp0User001".into(), vec![])), false)
            )
            .is_err()
        );
    }

    #[test]
    fn sharing_filter_for_users() {
        let a = Access {
            superuser: false,
            skip_sharing_check: false,
            can_read_type: true,
            user: Some((
                "Crp0User001".into(),
                vec!["Crp0Grp0001".into(), "Grp2".into()],
            )),
            authorities: vec![],
        };
        let p = plan_list(constant(), &params("id"), a.clone()).unwrap();
        assert!(p.fetch_sql.contains(
            "where (1=1) and (( ( jsonb_extract_path_text(e.sharing, 'owner') is null or jsonb_extract_path_text(e.sharing, 'owner') = 'Crp0User001')"
        ), "{}", p.fetch_sql);
        assert!(p.fetch_sql.contains(
            "jsonb_check_user_groups_access( e.sharing, 'r%', '{Crp0Grp0001,Grp2}') = true ))"
        ));
        let no_groups = Access {
            user: Some(("Crp0User001".into(), vec![])),
            ..a.clone()
        };
        let p = plan_list(constant(), &params("id"), no_groups).unwrap();
        assert!(!p.fetch_sql.contains("group"));
        let skip = Access {
            skip_sharing_check: true,
            user: None,
            ..a
        };
        let p = plan_list(constant(), &params("id"), skip).unwrap();
        assert!(p.fetch_sql.contains("where (1=1) and (1=1)"));
        let bad = Access {
            superuser: false,
            skip_sharing_check: false,
            can_read_type: true,
            user: Some(("x'y".into(), vec![])),
            authorities: vec![],
        };
        assert!(plan_list(constant(), &params("id"), bad).is_err());
    }

    #[test]
    fn collections_required_references_and_default_order() {
        let ug = registered::by_singular("userGroup").unwrap();
        let p = plan_list(ug, &params("id,users::size,users::ids"), sup()).unwrap();
        assert!(p.fetch_sql.contains(
            "(select count(j1.usergroupid) from usergroupmembers j1 where e.usergroupid = j1.usergroupid)"
        ), "{}", p.fetch_sql);
        assert!(p.fetch_sql.contains(
            "(select array_agg(i2.uid) from userinfo i2 cross join usergroupmembers j2 where e.usergroupid=j2.usergroupid and (i2.userinfoid in (j2.userid)) and 1=1)"
        ), "{}", p.fetch_sql);
        let os = registered::by_singular("optionSet").unwrap();
        let p = plan_list(os, &params("id,options::ids"), sup()).unwrap();
        assert!(p.fetch_sql.contains(
            "from optionvalue i1 cross join optionvalue j1 where e.optionsetid=j1.optionsetid and (i1.optionvalueid in (j1.optionvalueid))"
        ), "{}", p.fetch_sql);
        let de = registered::by_singular("dataElement").unwrap();
        let p = plan_list(de, &params("id,categoryCombo"), sup()).unwrap();
        assert!(p.fetch_sql.contains(
            "from dataelement e cross join categorycombo r1 where e.categorycomboid=r1.categorycomboid and (1=1)"
        ), "{}", p.fetch_sql);
        assert!(p.count_sql.is_none());
        let opt = registered::by_singular("option").unwrap();
        let p = plan_list(opt, &params("id"), sup()).unwrap();
        assert!(
            p.fetch_sql
                .contains("order by e.optionsetid asc, e.code asc"),
            "{}",
            p.fetch_sql
        );
    }

    #[test]
    fn href_adds_id_support_field() {
        let p = plan_list(constant(), &params("href"), sup()).unwrap();
        assert_eq!(p.output.len(), 1);
        assert_eq!(p.slots.len(), 2);
        assert!(p.fetch_sql.starts_with("select e.uid from constant e"));
    }
}
