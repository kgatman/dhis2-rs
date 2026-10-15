//! Port of the Gist API engine (`org.hisp.dhis.gist`, dhis-service-core), list queries only.
//!
//! Java turns a request into a `GistQuery`, expands it (`GistPlanner`), validates it
//! (`GistValidator`), writes HQL (`GistBuilder`), runs it and streams the rows as JSON
//! (`GistOutput`, json-tree's `JsonBuilder`). The metadata list endpoints (`/api/{type}`) hand
//! requests the Gist engine can answer to it (`GistBridge`, [`bridge`]).
//!
//! Here the same steps produce PostgreSQL directly ([`plan::plan_list`]) from the captured schemas
//! and the generated Hibernate mappings; the caller runs it and renders the rows with
//! [`output`]. Only part of the engine is ported: anything else makes [`plan::plan_list`] return
//! [`Unsupported`], and the request goes to Java. That includes every request Java rejects:
//! Rust never produces Gist error responses.

pub mod bridge;
pub mod output;
pub mod plan;

use regex::Regex;
use std::sync::LazyLock;

use crate::property_path::PropertyPath;

/// The request cannot be answered here (not ported, or Java would fail); let Java answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unsupported(pub &'static str);

pub type Result<T> = std::result::Result<T, Unsupported>;

/// `GistQuery.Comparison`, in declaration order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Comparison {
    Null,
    NotNull,
    Eq,
    Ieq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    In,
    NotIn,
    Empty,
    NotEmpty,
    Like,
    NotLike,
    StartsLike,
    NotStartsLike,
    EndsLike,
    NotEndsLike,
    Ilike,
    NotIlike,
    StartsWith,
    NotStartsWith,
    EndsWith,
    NotEndsWith,
    CanRead,
    CanWrite,
    CanDataRead,
    CanDataWrite,
    CanAccess,
}

impl Comparison {
    const ALL: [(Comparison, &'static str, &'static [&'static str]); 30] = [
        (Comparison::Null, "is null", &["null"]),
        (Comparison::NotNull, "is not null", &["!null"]),
        (Comparison::Eq, "=", &["eq"]),
        (Comparison::Ieq, "=", &["ieq"]),
        (Comparison::Ne, "!=", &["!eq", "ne", "neq"]),
        (Comparison::Lt, "<", &["lt"]),
        (Comparison::Le, "<=", &["le", "lte"]),
        (Comparison::Gt, ">", &["gt"]),
        (Comparison::Ge, ">=", &["ge", "gte"]),
        (Comparison::In, "in", &["in"]),
        (Comparison::NotIn, "not in", &["!in"]),
        (Comparison::Empty, "= 0", &["empty"]),
        (Comparison::NotEmpty, "> 0", &["!empty"]),
        (Comparison::Like, "like", &["like"]),
        (Comparison::NotLike, "not like", &["!like"]),
        (Comparison::StartsLike, "like", &["$like"]),
        (Comparison::NotStartsLike, "not like", &["!$like"]),
        (Comparison::EndsLike, "like", &["like$"]),
        (Comparison::NotEndsLike, "not like", &["!like$"]),
        (Comparison::Ilike, "like", &["ilike"]),
        (Comparison::NotIlike, "not like", &["!ilike"]),
        (Comparison::StartsWith, "like", &["$ilike", "startswith"]),
        (
            Comparison::NotStartsWith,
            "not like",
            &["!$ilike", "!startswith"],
        ),
        (Comparison::EndsWith, "like", &["ilike$", "endswith"]),
        (
            Comparison::NotEndsWith,
            "not like",
            &["!ilike$", "!endswith"],
        ),
        (Comparison::CanRead, "", &["canread"]),
        (Comparison::CanWrite, "", &["canwrite"]),
        (Comparison::CanDataRead, "", &["candataread"]),
        (Comparison::CanDataWrite, "", &["candatawrite"]),
        (Comparison::CanAccess, "", &["canaccess"]),
    ];

    /// `Comparison.parse(symbol)` (`toLowerCase()` with the JVM locale: only ASCII is taken
    /// here, where every locale agrees).
    pub fn parse(symbol: &str) -> Result<Comparison> {
        if !symbol.is_ascii() {
            return Err(Unsupported("non-ASCII filter operator"));
        }
        let s = symbol.to_ascii_lowercase();
        Self::ALL
            .iter()
            .find(|(_, _, symbols)| symbols.contains(&s.as_str()))
            .map(|(c, _, _)| *c)
            .ok_or(Unsupported("unknown filter operator"))
    }

    pub fn sql(self) -> &'static str {
        Self::ALL[self as usize].1
    }

    /// `symbols[0]`, as `Filter#toString` prints it.
    pub fn symbol(self) -> &'static str {
        Self::ALL[self as usize].2[0]
    }

    pub fn is_unary(self) -> bool {
        matches!(
            self,
            Self::Null | Self::NotNull | Self::Empty | Self::NotEmpty
        )
    }

    pub fn is_multi_value(self) -> bool {
        matches!(self, Self::In | Self::NotIn | Self::CanAccess)
    }

    pub fn is_order_compare(self) -> bool {
        matches!(self, Self::Eq | Self::Ieq | Self::Ne) || self.is_numeric_compare()
    }

    pub fn is_numeric_compare(self) -> bool {
        matches!(self, Self::Lt | Self::Le | Self::Ge | Self::Gt)
    }

    pub fn is_emptiness_compare(self) -> bool {
        matches!(self, Self::Empty | Self::NotEmpty)
    }

    pub fn is_string_compare(self) -> bool {
        self >= Self::Like && self < Self::CanRead
    }

    pub fn is_access_compare(self) -> bool {
        self >= Self::CanRead
    }

    pub fn is_case_insensitive(self) -> bool {
        self == Self::Ieq || (self >= Self::Ilike && self <= Self::NotEndsWith)
    }
}

/// `GistQuery.Filter`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filter {
    pub group: i32,
    pub path: PropertyPath,
    pub op: Comparison,
    pub values: Vec<String>,
}

/// `String#split(regex)` given the match ranges: trailing empty strings are removed (a string
/// without any match is returned whole, an empty input as one empty string).
fn java_split<'a>(s: &'a str, separators: &[(usize, usize)]) -> Vec<&'a str> {
    if separators.is_empty() {
        return vec![s];
    }
    let mut parts = Vec::new();
    let mut from = 0;
    for &(a, b) in separators {
        parts.push(&s[from..a]);
        from = b;
    }
    parts.push(&s[from..]);
    while parts.len() > 1 && parts.last() == Some(&"") {
        parts.pop();
    }
    if parts.len() == 1 && parts[0].is_empty() && !s.is_empty() {
        // Every part was empty (e.g. ","): Java returns an empty array.
        parts.clear();
    }
    parts
}

/// `getStrings(value, ",")`: `null`/empty gives no strings.
fn split_commas(value: &str) -> Vec<&str> {
    if value.is_empty() {
        return Vec::new();
    }
    let seps: Vec<(usize, usize)> = value.match_indices(',').map(|(i, _)| (i, i + 1)).collect();
    java_split(value, &seps)
}

static FILTER_SPLIT: LazyLock<Regex> = LazyLock::new(|| {
    // Java's `\d` is ASCII-only; its `.` stops at any line terminator (checked in `parse`).
    Regex::new(r"^(?:([0-9]+)[:~@]{1,2})?([^:~@]+)[:~@]{1,2}([^:~@]+)(?:[:~@]{1,2}(.*))?$")
        .expect("valid regex")
});

impl Filter {
    /// `Filter.ofList(filter)`: split at commas that are neither inside `[...]` nor inside
    /// `(...)` (the regex `,(?![^\[\]]*\]|[^\(\)]*\)|([a-zA-Z0-9]+,?)+\))`: its third branch
    /// only matches where the second does), then [`Filter::parse`] each.
    pub fn of_list(filter: &str) -> Result<Vec<Filter>> {
        if filter.is_empty() {
            return Ok(Vec::new());
        }
        let first_of = |rest: &str, open: char, close: char| {
            rest.chars().find(|c| *c == open || *c == close) == Some(close)
        };
        let seps: Vec<(usize, usize)> = filter
            .match_indices(',')
            .filter(|(i, _)| {
                let rest = &filter[i + 1..];
                !(first_of(rest, '[', ']') || first_of(rest, '(', ')'))
            })
            .map(|(i, _)| (i, i + 1))
            .collect();
        java_split(filter, &seps)
            .into_iter()
            .map(Filter::parse)
            .collect()
    }

    /// `Filter.parse(filter)`: `[group:]name:op[:value]` with `:`, `~` or `@` (one or two) as
    /// delimiters; a value in `[...]` is a list.
    pub fn parse(filter: &str) -> Result<Filter> {
        if filter.contains(['\n', '\r', '\u{85}', '\u{2028}', '\u{2029}']) {
            return Err(Unsupported("line terminator in filter"));
        }
        let c = FILTER_SPLIT
            .captures(filter)
            .ok_or(Unsupported("invalid filter expression"))?;
        let group = match c.get(1) {
            Some(g) => g
                .as_str()
                .parse::<i32>()
                .map_err(|_| Unsupported("filter group out of range"))?,
            None => -1,
        };
        let path = PropertyPath::of(&c[2]).map_err(|_| Unsupported("invalid filter path"))?;
        let op = Comparison::parse(&c[3])?;
        let values = match c.get(4).map(|m| m.as_str()) {
            None | Some("") => Vec::new(),
            Some(v) if !(v.starts_with('[') && v.ends_with(']')) => vec![v.to_string()],
            // A lone "[" both starts and ends with a bracket; Java's substring(1, 0) fails.
            Some("[") => return Err(Unsupported("invalid filter list")),
            Some(v) => {
                let inner = &v[1..v.len() - 1];
                let seps: Vec<(usize, usize)> =
                    inner.match_indices(',').map(|(i, _)| (i, i + 1)).collect();
                java_split(inner, &seps)
                    .into_iter()
                    .map(String::from)
                    .collect()
            }
        };
        Ok(Filter {
            group,
            path,
            op,
            values,
        })
    }
}

/// `GistQuery.Order`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Order {
    pub path: PropertyPath,
    pub desc: bool,
}

impl Order {
    /// `Order.ofList(order)`.
    pub fn of_list(order: &str) -> Result<Vec<Order>> {
        split_commas(order).into_iter().map(Order::parse).collect()
    }

    /// `Order.parse(order)`: `name` or `name<delim>asc|desc` (delimiters `::`, `:`, `~`, `@`).
    pub fn parse(order: &str) -> Result<Order> {
        let mut seps = Vec::new();
        let b = order.as_bytes();
        let mut i = 0;
        while i < b.len() {
            if b[i] == b':' && b.get(i + 1) == Some(&b':') {
                seps.push((i, i + 2));
                i += 2;
            } else if matches!(b[i], b':' | b'~' | b'@') {
                seps.push((i, i + 1));
                i += 1;
            } else {
                i += 1;
            }
        }
        let parts = java_split(order, &seps);
        match parts.as_slice() {
            // Java parses the whole expression as the path here (e.g. "name:" fails).
            [_] => Ok(Order {
                path: PropertyPath::of(order).map_err(|_| Unsupported("invalid order path"))?,
                desc: false,
            }),
            [name, dir] => {
                if !dir.is_ascii() {
                    return Err(Unsupported("non-ASCII order direction"));
                }
                let desc = match dir.to_ascii_uppercase().as_str() {
                    "ASC" => false,
                    "DESC" => true,
                    _ => return Err(Unsupported("invalid order direction")),
                };
                Ok(Order {
                    path: PropertyPath::of(name).map_err(|_| Unsupported("invalid order path"))?,
                    desc,
                })
            }
            _ => Err(Unsupported("invalid order expression")),
        }
    }
}

/// `GistAutoType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoType {
    Xl,
    L,
    M,
    S,
    Xs,
}

impl AutoType {
    /// `Enum.valueOf` (exact constant name).
    pub fn parse(s: &str) -> Option<AutoType> {
        Some(match s {
            "XL" => Self::Xl,
            "L" => Self::L,
            "M" => Self::M,
            "S" => Self::S,
            "XS" => Self::Xs,
            _ => return None,
        })
    }

    /// `GistAutoType#isIncluded`: the property type (and item type of collections) is in the
    /// included set.
    pub fn is_included(self, p: &dhis_schema::Property) -> bool {
        let includes = |t: Option<&str>| {
            let Some(t) = t else { return false };
            match self {
                Self::Xl | Self::L | Self::M => t != "PASSWORD",
                Self::S => t != "PASSWORD" && t != "COMPLEX",
                Self::Xs => matches!(t, "IDENTIFIER" | "TEXT" | "EMAIL"),
            }
        };
        includes(Some(p.property_type.name()))
            && (!p.collection || includes(p.item_property_type.as_ref().map(|t| t.name())))
    }

    /// `getDefaultTransformation()`.
    pub fn default_transformation(self) -> crate::fields::Transform {
        use crate::fields::Transform;
        match self {
            Self::Xl => Transform::IdObjects,
            Self::L => Transform::Ids,
            Self::M => Transform::Size,
            Self::S | Self::Xs => Transform::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_lists_like_java() {
        let f = Filter::of_list("name:eq:a,id:in:[x,y],code:like:b").unwrap();
        assert_eq!(f.len(), 3);
        assert_eq!(f[1].values, ["x", "y"]);
        assert_eq!(f[1].op, Comparison::In);
        assert_eq!(f[0].group, -1);
        let g = Filter::parse("1:name:!null").unwrap();
        assert_eq!((g.group, g.op, g.values.len()), (1, Comparison::NotNull, 0));
        // a value may contain delimiters after the operator
        let v = Filter::parse("name:eq:a:b~c").unwrap();
        assert_eq!(v.values, ["a:b~c"]);
        assert_eq!(Filter::parse("name~ilike~x").unwrap().op, Comparison::Ilike);
        // "[]" is one empty value, trailing empty values are dropped
        assert_eq!(Filter::parse("id:in:[]").unwrap().values, [""]);
        assert_eq!(Filter::parse("id:in:[a,,]").unwrap().values, ["a"]);
        assert!(Filter::of_list(",name:eq:a").is_err());
        assert!(Filter::parse("name").is_err());
        assert!(Filter::parse("name:bogus:1").is_err());
        assert_eq!(
            Comparison::parse("StartsWith").unwrap(),
            Comparison::StartsWith
        );
    }

    #[test]
    fn orders_like_java() {
        let o = Order::of_list("name,code:desc,id::asc,value~DESC").unwrap();
        assert_eq!(o.len(), 4);
        assert!(!o[0].desc && o[1].desc && !o[2].desc && o[3].desc);
        assert!(Order::parse("name:").is_err());
        assert!(Order::parse("name:up").is_err());
        assert!(Order::parse("a:b:c").is_err());
        assert!(Order::of_list("").unwrap().is_empty());
    }

    #[test]
    fn comparison_classes() {
        assert!(Comparison::Like.is_string_compare());
        assert!(!Comparison::CanRead.is_string_compare());
        assert!(Comparison::NotEndsWith.is_case_insensitive());
        assert!(!Comparison::NotEndsLike.is_case_insensitive());
        assert_eq!(Comparison::Ne.sql(), "!=");
        assert_eq!(Comparison::StartsWith.symbol(), "$ilike");
    }
}
