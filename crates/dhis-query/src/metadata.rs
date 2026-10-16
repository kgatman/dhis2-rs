//! The metadata query engine (`QueryService`) for list requests that the `GistBridge` does not
//! hand to Gist: `DefaultQueryParser` (filters), `Order.parse`, `DefaultQueryPlanner` (database or
//! in-memory) and `JpaCriteriaQueryEngine` (the SQL).
//!
//! Only plans whose every filter and order runs in the database are built; anything the planner
//! would leave to the in-memory engine, and every request Java answers with an error, is
//! [`Unsupported`] (the caller hands it to Java).
//!
//! Supported filters: root properties that are persisted, simple and mapped to a plain column of
//! type `String`, `Integer`, `Double`, `Boolean` or one of [`CHECKED_ENUMS`], with the operators
//! `eq`, `ieq`, `!eq`/`ne`/`neq`, `gt`, `lt`, `ge`/`gte`, `le`/`lte`, the `like`/`ilike` family,
//! `in`, `!in`, `null`, `!null`; and the `query` parameter (`Filters.query`).

use dhis_schema::hibernate::Entity;
use dhis_schema::registered::RegisteredSchema;
use dhis_schema::{Property, resolve_translation_base_property_name};

use crate::gist::plan::SqlParam;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unsupported(pub &'static str);

type Result<T> = std::result::Result<T, Unsupported>;

/// Enums whose `toString()` is their `name()` (read from their source): the schema's `constants`
/// are then the names `Enums.getIfPresent` accepts and `EnumType.STRING` stores.
pub const CHECKED_ENUMS: &[&str] = &[
    "org.hisp.dhis.analytics.AggregationType",
    "org.hisp.dhis.common.DataDimensionType",
    "org.hisp.dhis.common.ValueType",
    "org.hisp.dhis.dataelement.DataElementDomain",
];

/// `AclService.LIKE_READ_METADATA`.
const LIKE_READ_METADATA: &str = "r%";

/// What a list query needs besides the request's filters and orders.
#[derive(Debug, Clone)]
pub struct ListQuery<'a> {
    pub schema: &'static RegisteredSchema,
    pub entity: &'static Entity,
    /// `GetObjectListParams#getFilters()` (already split).
    pub filters: &'a [String],
    /// `query` (`Filters.query`), when not empty.
    pub query: Option<&'a str>,
    /// The `order` values (already split).
    pub orders: &'a [String],
    /// `rootJunction=OR`.
    pub any: bool,
    /// The user's database locale (`UserSettings#getUserDbLocale`), as `Locale#toString`.
    pub db_locale: &'a str,
    /// The sharing check's user (uid, group uids); `None` for superusers.
    pub sharing_user: Option<(&'a str, &'a [String])>,
}

/// The `where` and `order by` clauses (on alias `e`) with their bound values (`$1`...).
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub where_sql: String,
    pub order_sql: String,
    pub params: Vec<SqlParam>,
}

/// `GetObjectListParams.splitFilters`: the comma-joined `filter` values split into filters.
pub fn split_filters(filters: &str) -> Vec<String> {
    if filters.is_empty() {
        return Vec::new();
    }
    if !filters.contains(',') {
        return vec![filters.to_string()];
    }
    // FILTER_PARTS: (?<property>[a-zA-Z0-9.]+):(?<op>[!$a-zA-Z]{2,10})(?::(?<value>\[[^]]*]|[^,\n\r]+))?
    let b = filters.as_bytes();
    let mut out = Vec::new();
    let mut start = 0;
    while start < b.len() {
        match match_filter_at(b, start) {
            Some(end) => {
                out.push(filters[start..end].to_string());
                start = end.max(start + 1);
            }
            None => start += 1,
        }
    }
    out
}

/// The end of a `FILTER_PARTS` match starting at `i` (greedy, with Java's backtracking where it
/// matters: the property and operator runs are followed by fixed characters).
fn match_filter_at(b: &[u8], i: usize) -> Option<usize> {
    let prop = |c: u8| c.is_ascii_alphanumeric() || c == b'.';
    let op = |c: u8| c.is_ascii_alphabetic() || c == b'!' || c == b'$';
    let mut j = i;
    while j < b.len() && prop(b[j]) {
        j += 1;
    }
    // the property run cannot give back characters usefully: ':' is no property character
    if j == i || j >= b.len() || b[j] != b':' {
        return None;
    }
    j += 1;
    let op_start = j;
    while j < b.len() && op(b[j]) && j - op_start < 10 {
        j += 1;
    }
    let op_len = j - op_start;
    if op_len < 2 {
        return None;
    }
    // optional value: ':' then '[...]' or a run without ',', '\n', '\r'
    if j < b.len() && b[j] == b':' {
        let v = j + 1;
        if v < b.len()
            && b[v] == b'['
            && let Some(close) = b[v + 1..].iter().position(|&c| c == b']')
        {
            return Some(v + 1 + close + 1);
        }
        let mut k = v;
        while k < b.len() && !matches!(b[k], b',' | b'\n' | b'\r') {
            k += 1;
        }
        if k > v {
            return Some(k);
        }
    }
    // an operator run longer than 10 would have been cut: the regex then needs the next
    // character to be ':' or the end of a match, which it is not when more operator characters
    // follow; the greedy run ends there anyway (a match ends without a value)
    Some(j)
}

/// The `order` parameter values (`List<String>` binding): one value is split at commas and each
/// part trimmed (`StringToCollectionConverter`); several values are taken as they are.
pub fn split_orders(values: &[&str]) -> Vec<String> {
    match values {
        [] => Vec::new(),
        [single] => {
            if single.is_empty() {
                return Vec::new();
            }
            single
                .split(',')
                .map(|p| java_trim(p).to_string())
                .collect()
        }
        many => many.iter().map(|v| v.to_string()).collect(),
    }
}

/// `String#trim`: strips characters up to U+0020 at both ends.
fn java_trim(s: &str) -> &str {
    s.trim_matches(|c: char| c <= ' ')
}

/// `String#split(regex)` for a one-character literal: trailing empty strings removed (an empty
/// input gives `[""]`).
fn java_split(s: &str, sep: char) -> Vec<&str> {
    if s.is_empty() {
        return vec![""];
    }
    let mut parts: Vec<&str> = s.split(sep).collect();
    while parts.len() > 1 && parts.last() == Some(&"") {
        parts.pop();
    }
    if parts.len() == 1 && parts[0].is_empty() {
        // "::" splits into nothing at all
        return Vec::new();
    }
    parts
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ValueType {
    Text,
    Int,
    Double,
    Bool,
    Enum,
}

struct Column<'a> {
    sql: String,
    ty: ValueType,
    property: &'a Property,
}

/// A root property's plain column (persisted, simple, not a collection).
fn column<'p>(entity: &Entity, p: &'p Property) -> Result<Column<'p>> {
    if !p.persisted || !p.simple || p.collection || p.embedded_object {
        return Err(Unsupported("in-memory filter"));
    }
    let field = p.field_name.as_deref().ok_or(Unsupported("no field"))?;
    let m = entity
        .properties
        .get(field)
        .ok_or(Unsupported("property not mapped"))?;
    if m.kind != "property" || m.formula || m.column_table.is_some() {
        return Err(Unsupported("mapping kind"));
    }
    let col = m.column.as_deref().ok_or(Unsupported("no column"))?;
    let ty = if p.property_type.name() == "CONSTANT" {
        let checked = p
            .klass
            .as_deref()
            .is_some_and(|k| CHECKED_ENUMS.contains(&k));
        if !checked || m.hibernate_type.as_deref() != Some("enum-string") {
            return Err(Unsupported("enum mapping"));
        }
        ValueType::Enum
    } else {
        let (ty, ok) = match p.klass.as_deref() {
            Some("java.lang.String") => (ValueType::Text, ["text", "string"]),
            Some("java.lang.Integer") => (ValueType::Int, ["integer", "integer"]),
            Some("java.lang.Double") => (ValueType::Double, ["double", "double"]),
            Some("java.lang.Boolean") => (ValueType::Bool, ["boolean", "boolean"]),
            // dates (DateUtils.parseDate) and primitives (no parser: 400) are not ported
            _ => return Err(Unsupported("property type")),
        };
        if !m.hibernate_type.as_deref().is_none_or(|t| ok.contains(&t)) {
            return Err(Unsupported("hibernate type"));
        }
        ty
    };
    Ok(Column {
        sql: format!("e.{col}"),
        ty,
        property: p,
    })
}

/// `QueryUtils.parseValue(klass, value)` for the supported types; `None` where Java fails.
fn parse_value(c: &Column<'_>, v: &str) -> Option<SqlParam> {
    Some(match c.ty {
        ValueType::Text => SqlParam::Text(v.to_string()),
        // Integer.valueOf: optional sign, decimal digits (ASCII only here)
        ValueType::Int => {
            let digits = v.strip_prefix(['+', '-']).unwrap_or(v);
            if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            SqlParam::Int(v.parse().ok()?)
        }
        // Double.valueOf accepts more (hex, suffixes, whitespace, NaN): only plain decimals here
        ValueType::Double => {
            let s = v.strip_prefix(['+', '-']).unwrap_or(v);
            let (mantissa, exp) = match s.find(['e', 'E']) {
                Some(i) => (&s[..i], Some(&s[i + 1..])),
                None => (s, None),
            };
            let (int, frac) = mantissa.split_once('.').unwrap_or((mantissa, ""));
            let digits_ok = |d: &str| d.bytes().all(|b| b.is_ascii_digit());
            let exp_ok = exp.is_none_or(|e| {
                let e = e.strip_prefix(['+', '-']).unwrap_or(e);
                !e.is_empty() && digits_ok(e)
            });
            if (int.is_empty() && frac.is_empty()) || !digits_ok(int) || !digits_ok(frac) || !exp_ok
            {
                return None;
            }
            let d: f64 = v.parse().ok()?;
            if !d.is_finite() {
                return None;
            }
            SqlParam::Double(d)
        }
        // Boolean.valueOf: "true" in any case, anything else false
        ValueType::Bool => SqlParam::Bool(v.eq_ignore_ascii_case("true")),
        ValueType::Enum => {
            let names = c.property.constants.as_ref()?;
            if !names.iter().any(|n| n == v) {
                return None;
            }
            SqlParam::Text(v.to_string())
        }
    })
}

/// `String#toLowerCase(locale)` where every locale agrees (ASCII without `I`, which Turkish
/// and Azeri lower differently); `None` otherwise.
fn lower_safe(v: &str) -> Option<String> {
    (v.is_ascii() && !v.contains('I')).then(|| v.to_ascii_lowercase())
}

struct Builder {
    params: Vec<SqlParam>,
}

impl Builder {
    fn bind(&mut self, p: SqlParam) -> String {
        self.params.push(p);
        format!("${}", self.params.len())
    }
}

#[derive(Clone, Copy)]
enum Like {
    Exact,
    Start,
    End,
    Anywhere,
}

impl Like {
    fn pattern(self, v: &str) -> String {
        match self {
            Like::Exact => v.to_string(),
            Like::Start => format!("{v}%"),
            Like::End => format!("%{v}"),
            Like::Anywhere => format!("%{v}%"),
        }
    }
}

/// One `filter` (`DefaultQueryParser#parse` + the operator's predicate).
fn filter_sql(q: &ListQuery<'_>, filter: &str, b: &mut Builder) -> Result<String> {
    let parts = java_split(filter, ':');
    if parts.len() < 2 {
        return Err(Unsupported("invalid filter"));
    }
    let (path, op) = (parts[0], parts[1]);
    let arg = (parts.len() >= 3).then(|| parts[2..].join(":"));
    if path == "identifiable" || path == "mentions" || path.contains('.') {
        return Err(Unsupported("filter path"));
    }
    let p = q
        .schema
        .property(path)
        .ok_or(Unsupported("unknown filter property"))?;
    let c = column(q.entity, p)?;
    let value = |b: &mut Builder| -> Result<String> {
        let a = arg.as_deref().ok_or(Unsupported("filter without value"))?;
        let v = parse_value(&c, a).ok_or(Unsupported("filter value"))?;
        Ok(b.bind(v))
    };
    let like = |b: &mut Builder, case_sensitive: bool, mode: Like, not: bool| -> Result<String> {
        if c.ty != ValueType::Text {
            return Err(Unsupported("like on non-text"));
        }
        let a = arg.as_deref().ok_or(Unsupported("filter without value"))?;
        let a = a.replace('%', "");
        let (lhs, v) = if case_sensitive {
            (c.sql.clone(), a)
        } else {
            (
                format!("lower({})", c.sql),
                lower_safe(&a).ok_or(Unsupported("locale-dependent lower case"))?,
            )
        };
        let rhs = b.bind(SqlParam::Text(mode.pattern(&v)));
        Ok(match (mode, not) {
            (Like::Exact, false) => format!("{lhs} = {rhs}"),
            (Like::Exact, true) => format!("{lhs} <> {rhs}"),
            (_, false) => format!("{lhs} like {rhs}"),
            (_, true) => format!("{lhs} not like {rhs}"),
        })
    };
    let compare = |b: &mut Builder, sym: &str| -> Result<String> {
        if c.ty == ValueType::Enum && sym != "=" && sym != "<>" {
            // not among the comparison operators' types: 400
            return Err(Unsupported("enum comparison"));
        }
        Ok(format!("{} {sym} {}", c.sql, value(b)?))
    };
    match op {
        "eq" => compare(b, "="),
        "!eq" | "neq" | "ne" => compare(b, "<>"),
        "gt" => compare(b, ">"),
        "lt" => compare(b, "<"),
        "gte" | "ge" => compare(b, ">="),
        "lte" | "le" => compare(b, "<="),
        "ieq" => like(b, false, Like::Exact, false),
        "like" => like(b, true, Like::Anywhere, false),
        "!like" => like(b, true, Like::Anywhere, true),
        "$like" => like(b, true, Like::Start, false),
        "!$like" => like(b, true, Like::Start, true),
        "like$" => like(b, true, Like::End, false),
        "!like$" => like(b, true, Like::End, true),
        "ilike" => like(b, false, Like::Anywhere, false),
        "!ilike" => like(b, false, Like::Anywhere, true),
        "startsWith" | "$ilike" => like(b, false, Like::Start, false),
        "!$ilike" => like(b, false, Like::Start, true),
        "endsWith" | "ilike$" => like(b, false, Like::End, false),
        "!ilike$" => like(b, false, Like::End, true),
        "in" | "!in" => {
            let a = arg.as_deref().ok_or(Unsupported("filter without value"))?;
            let inner = a
                .strip_prefix('[')
                .and_then(|r| r.strip_suffix(']'))
                .ok_or(Unsupported("in without brackets"))?;
            let mut items = Vec::new();
            for item in java_split(inner, ',') {
                let v = parse_value(&c, item).ok_or(Unsupported("filter value"))?;
                items.push(b.bind(v));
            }
            if items.is_empty() {
                return Err(Unsupported("empty in"));
            }
            let in_sql = format!("{} in ({})", c.sql, items.join(", "));
            Ok(if op == "in" {
                in_sql
            } else {
                format!("not ({in_sql})")
            })
        }
        "null" => Ok(format!("{} is null", c.sql)),
        "!null" => Ok(format!("{} is not null", c.sql)),
        _ => Err(Unsupported("filter operator")),
    }
}

/// `JpaCriteriaQueryEngine#buildQueryFilter`: uid or code equal, or the name containing the
/// value ignoring case (`%` is not stripped here).
fn query_sql(q: &ListQuery<'_>, value: &str, b: &mut Builder) -> Result<String> {
    let col = |name: &str| -> Result<String> {
        let p = q
            .schema
            .property(name)
            .ok_or(Unsupported("query property"))?;
        let field = p.field_name.as_deref().ok_or(Unsupported("no field"))?;
        let m = q
            .entity
            .properties
            .get(field)
            .filter(|m| m.kind == "property" && !m.formula && m.column_table.is_none())
            .and_then(|m| m.column.as_deref())
            .ok_or(Unsupported("query column"))?;
        Ok(format!("e.{m}"))
    };
    let (uid, code, name) = (col("id")?, col("code")?, col("name")?);
    let lower = lower_safe(value).ok_or(Unsupported("locale-dependent lower case"))?;
    let v = b.bind(SqlParam::Text(value.to_string()));
    let pat = b.bind(SqlParam::Text(format!("%{lower}%")));
    Ok(format!(
        "({uid} = {v} or {code} = {v} or lower({name}) like {pat})"
    ))
}

/// A value embedded in SQL (uids, locales, translation keys): word characters only.
fn literal_safe(s: &str) -> Result<&str> {
    if !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        Ok(s)
    } else {
        Err(Unsupported("unusual literal"))
    }
}

/// `InternalHibernateGenericStoreImpl#getSharingPredicates` (`r%`).
fn sharing_sql(entity: &Entity, uid: &str, groups: &[String]) -> Result<String> {
    let col = entity
        .properties
        .get("sharing")
        .filter(|m| m.kind == "property")
        .and_then(|m| m.column.as_deref())
        .ok_or(Unsupported("no sharing column"))?;
    let s = format!("e.{col}");
    let uid = literal_safe(uid)?;
    let public = format!("jsonb_extract_path_text({s}, 'public')");
    let owner = format!("jsonb_extract_path_text({s}, 'owner')");
    let a = LIKE_READ_METADATA;
    let mut sql = format!(
        "({public} like '{a}' or {public} = 'null' or {public} is null or {owner} is null \
         or {owner} = 'null' or {owner} = '{uid}' \
         or (jsonb_has_user_id({s}, '{uid}') = true and jsonb_check_user_access({s}, '{uid}', '{a}') = true)"
    );
    if !groups.is_empty() {
        for g in groups {
            literal_safe(g)?;
        }
        let ids = format!("{{{}}}", groups.join(","));
        sql.push_str(&format!(
            " or (jsonb_has_user_group_ids({s}, '{ids}') = true and jsonb_check_user_groups_access({s}, '{a}', '{ids}') = true)"
        ));
    }
    sql.push(')');
    Ok(sql)
}

/// `Order`: property, ascending, ignore case.
struct Order<'a> {
    property: &'a str,
    asc: bool,
    ignore_case: bool,
}

/// `Order.parse`: `property[:direction]`, the first order of a property wins.
fn parse_orders(orders: &[String]) -> Vec<Order<'_>> {
    let mut seen: Vec<&str> = Vec::new();
    let mut out = Vec::new();
    for o in orders {
        let parts = java_split(o, ':');
        if parts.is_empty() {
            // an all-colon string splits into nothing: skipped
            continue;
        }
        let direction = if parts.len() == 2 {
            parts[1].to_ascii_lowercase()
        } else {
            "asc".to_string()
        };
        let property = parts[0];
        if seen.contains(&property) {
            continue;
        }
        seen.push(property);
        let (asc, ignore_case) = match direction.as_str() {
            "iasc" => (true, true),
            "desc" => (false, false),
            "idesc" => (false, true),
            _ => (true, false),
        };
        out.push(Order {
            property,
            asc,
            ignore_case,
        });
    }
    out
}

fn can_be_translated(p: &Property) -> bool {
    p.translatable && p.translation_key.is_some()
}

/// The plain column of a property, looked up by its name (`root.get(name)`) or field name.
fn plain_column(entity: &Entity, field: &str) -> Result<String> {
    entity
        .properties
        .get(field)
        .filter(|m| m.kind == "property" && !m.formula && m.column_table.is_none())
        .and_then(|m| m.column.as_deref())
        .map(|c| format!("e.{c}"))
        .ok_or(Unsupported("order column"))
}

/// `JpaCriteriaQueryEngine#getOrderPredicate`.
fn order_sql(q: &ListQuery<'_>, o: &Order<'_>) -> Result<String> {
    let schema = q.schema;
    let p = schema
        .property(o.property)
        .ok_or(Unsupported("unknown order property"))?;
    let translated = |bp: &Property| -> Result<String> {
        let key = literal_safe(bp.translation_key.as_deref().unwrap_or(""))?;
        let tr = plain_column(q.entity, "translations")?;
        let base = plain_column(q.entity, &resolve_translation_base_property_name(&bp.name))?;
        let locale = literal_safe(q.db_locale)?;
        let v = format!("coalesce(jsonb_get_translated_value({tr}, '{key}', '{locale}'), {base})");
        Ok(if o.ignore_case {
            format!("lower({v})")
        } else {
            v
        })
    };
    let regular = |rp: &Property| -> Result<String> {
        let field = rp.field_name.as_deref().ok_or(Unsupported("no field"))?;
        let col = plain_column(q.entity, field)?;
        let text = matches!(rp.property_type.name(), "TEXT" | "EMAIL" | "USERNAME");
        Ok(if o.ignore_case && text {
            format!("lower({col})")
        } else {
            col
        })
    };
    let expr = if o.property.starts_with("display") {
        let base = resolve_translation_base_property_name(o.property);
        match schema.property(&base) {
            Some(bp) if can_be_translated(bp) => translated(bp)?,
            Some(bp) => regular(bp)?,
            None => regular(p)?,
        }
    } else if can_be_translated(p) {
        translated(p)?
    } else {
        regular(p)?
    };
    Ok(format!("{expr} {}", if o.asc { "asc" } else { "desc" }))
}

/// The plan of `queryService.query(query)` (and `count`, which uses its `where`).
pub fn plan(q: &ListQuery<'_>) -> Result<Plan> {
    let mut b = Builder { params: Vec::new() };
    let mut filters = Vec::new();
    for f in q.filters {
        filters.push(filter_sql(q, f, &mut b)?);
    }
    if let Some(v) = q.query {
        filters.push(query_sql(q, v, &mut b)?);
    }
    let mut clauses = Vec::new();
    if !filters.is_empty() {
        let junction = if q.any { " or " } else { " and " };
        clauses.push(format!("({})", filters.join(junction)));
    }
    if let Some((uid, groups)) = q.sharing_user
        && q.schema.shareable
    {
        clauses.push(sharing_sql(q.entity, uid, groups)?);
    }
    let where_sql = if clauses.is_empty() {
        "1=1".to_string()
    } else {
        clauses.join(" and ")
    };

    // Query#setDefaultOrder + DefaultQueryPlanner#autoFill: name (ignore case), then id
    let parsed = parse_orders(q.orders);
    let orders = if parsed.is_empty() {
        let mut d = Vec::new();
        if q.schema.property("name").is_some_and(|p| p.persisted) {
            d.push(Order {
                property: "name",
                asc: true,
                ignore_case: true,
            });
        }
        if q.schema.property("id").is_some_and(|p| p.persisted) {
            d.push(Order {
                property: "id",
                asc: true,
                ignore_case: false,
            });
        }
        d
    } else {
        parsed
    };
    // DefaultQueryPlanner#split: database ordering only if every property is simple and
    // persisted (or a display property)
    for o in &orders {
        let ok = q
            .schema
            .property(o.property)
            .is_some_and(|p| (p.persisted || p.name.starts_with("display")) && p.simple);
        if !ok {
            return Err(Unsupported("in-memory order"));
        }
    }
    let order_sql = orders
        .iter()
        .map(|o| order_sql(q, o))
        .collect::<Result<Vec<_>>>()?
        .join(", ");
    Ok(Plan {
        where_sql,
        order_sql,
        params: b.params,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use dhis_schema::{hibernate, registered};

    #[test]
    fn filters_split_like_java() {
        assert_eq!(split_filters("name:eq:a"), ["name:eq:a"]);
        assert_eq!(
            split_filters("name:eq:a,id:in:[x,y],code:!null"),
            ["name:eq:a", "id:in:[x,y]", "code:!null"]
        );
        assert_eq!(split_filters("name:like:a b,x"), ["name:like:a b"]);
        // measured with the pattern in jshell (JDK 17)
        assert_eq!(split_filters("a:in:[x,y,b:eq:1"), ["a:in:[x", "b:eq:1"]);
        assert_eq!(
            split_filters("id:abcdefghijklm:x,b:eq:2"),
            ["id:abcdefghij", "b:eq:2"]
        );
        assert_eq!(split_filters("x:eq:,y:ne:3"), ["x:eq", "y:ne:3"]);
        assert_eq!(split_filters(":eq:a,b.c:eq:[a]]"), ["b.c:eq:[a]"]);
        assert!(split_filters("").is_empty());
    }

    #[test]
    fn java_split_drops_trailing_empties() {
        assert_eq!(java_split("a:b::", ':'), ["a", "b"]);
        assert_eq!(java_split("a::b", ':'), ["a", "", "b"]);
        assert_eq!(java_split("", ','), [""]);
        assert!(java_split("::", ':').is_empty());
    }

    fn query<'a>(
        singular: &str,
        filters: &'a [String],
        orders: &'a [String],
        q: Option<&'a str>,
    ) -> ListQuery<'a> {
        let schema = registered::by_singular(singular).unwrap();
        ListQuery {
            schema,
            entity: hibernate::entity(&schema.klass).unwrap(),
            filters,
            query: q,
            orders,
            any: false,
            db_locale: "fr",
            sharing_user: None,
        }
    }

    #[test]
    fn plans_like_the_jpa_engine() {
        let f = vec!["value:gt:1.5".to_string(), "name:ilike:Corp".to_string()];
        let p = plan(&query("constant", &f, &[], Some("x"))).unwrap();
        assert_eq!(
            p.where_sql,
            "(e.value > $1 and lower(e.name) like $2 and (e.uid = $3 or e.code = $3 or lower(e.name) like $4))"
        );
        assert_eq!(
            p.order_sql,
            "lower(coalesce(jsonb_get_translated_value(e.translations, 'NAME', 'fr'), e.name)) asc, e.uid asc"
        );
        assert_eq!(
            p.params,
            [
                SqlParam::Double(1.5),
                SqlParam::Text("%corp%".into()),
                SqlParam::Text("x".into()),
                SqlParam::Text("%x%".into())
            ]
        );
        let o = vec!["code:idesc".to_string(), "code:asc".to_string()];
        let p = plan(&query("constant", &[], &o, None)).unwrap();
        // `code` is an IDENTIFIER: ignoring case only applies to TEXT, EMAIL and USERNAME
        assert_eq!(p.order_sql, "e.code desc");
        let o = vec![
            "shortName:idesc".to_string(),
            "displayDescription".to_string(),
        ];
        let p = plan(&query("constant", &[], &o, None)).unwrap();
        assert_eq!(
            p.order_sql,
            "lower(coalesce(jsonb_get_translated_value(e.translations, 'SHORT_NAME', 'fr'), e.shortname)) desc, \
             coalesce(jsonb_get_translated_value(e.translations, 'DESCRIPTION', 'fr'), e.description) asc"
        );
        let f = vec!["valueType:in:[NUMBER,TEXT]".to_string()];
        let p = plan(&query("dataElement", &f, &[], None)).unwrap();
        assert_eq!(p.where_sql, "(e.valuetype in ($1, $2))");
        for bad in [
            "valueType:eq:number",
            "name:token:x",
            "value:like:1",
            "name:eq",
        ] {
            let f = vec![bad.to_string()];
            assert!(plan(&query("dataElement", &f, &[], None)).is_err(), "{bad}");
        }
    }
}
