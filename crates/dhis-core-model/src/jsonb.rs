//! Values DHIS2 stores in JSONB columns (sharing, translations, attribute values), read as its
//! Hibernate types read them and written as the Web API writes them.
//!
//! Reading goes through Jackson into Java objects, which decides what survives: `Sharing`
//! (`SharingJsonBinaryType`, absent maps become empty, an access entry's `displayName` is
//! read-only and so always `null`; maps are `LinkedHashMap`s in stored order), translations
//! (`JsonSetBinaryType`: a `HashSet<Translation>`, so iteration follows `Translation#hashCode`;
//! the locale is DHIS2's `Locale`, invalid ones fail), attribute values (`AttributeValues.of`).
//! The input is the column's text as PostgreSQL returns it (`jsonb` normalises key order).
//! Checked against the Java classes run on the server image (`tools/gen-jsonb-fixtures.sh`).

use serde::Deserialize;
use serde::de::{Deserializer, MapAccess, SeqAccess, Visitor};

use crate::java::hash::{self, Construction};
use crate::java::json::write_string as jackson_string;
use crate::locale::Locale;

/// JSON with object members in document order.
#[derive(Debug, Clone, PartialEq)]
pub enum Ordered {
    Null,
    Bool(bool),
    Number(serde_json::Number),
    String(String),
    Array(Vec<Ordered>),
    Object(Vec<(String, Ordered)>),
}

impl<'de> Deserialize<'de> for Ordered {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Ordered;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("JSON")
            }
            fn visit_unit<E>(self) -> Result<Ordered, E> {
                Ok(Ordered::Null)
            }
            fn visit_bool<E>(self, v: bool) -> Result<Ordered, E> {
                Ok(Ordered::Bool(v))
            }
            fn visit_i64<E>(self, v: i64) -> Result<Ordered, E> {
                Ok(Ordered::Number(v.into()))
            }
            fn visit_u64<E>(self, v: u64) -> Result<Ordered, E> {
                Ok(Ordered::Number(v.into()))
            }
            fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Ordered, E> {
                serde_json::Number::from_f64(v)
                    .map(Ordered::Number)
                    .ok_or_else(|| E::custom("non-finite number"))
            }
            fn visit_str<E>(self, v: &str) -> Result<Ordered, E> {
                Ok(Ordered::String(v.to_string()))
            }
            fn visit_string<E>(self, v: String) -> Result<Ordered, E> {
                Ok(Ordered::String(v))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Ordered, A::Error> {
                let mut out = Vec::new();
                while let Some(v) = seq.next_element()? {
                    out.push(v);
                }
                Ok(Ordered::Array(out))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Ordered, A::Error> {
                let mut out: Vec<(String, Ordered)> = Vec::new();
                while let Some((k, v)) = map.next_entry::<String, Ordered>()? {
                    // a repeated key: Jackson keeps the last value at the first position
                    match out.iter_mut().find(|(e, _)| *e == k) {
                        Some(e) => e.1 = v,
                        None => out.push((k, v)),
                    }
                }
                Ok(Ordered::Object(out))
            }
        }
        d.deserialize_any(V)
    }
}

impl Ordered {
    pub fn parse(json: &str) -> Option<Ordered> {
        serde_json::from_str(json).ok()
    }

    pub fn get(&self, key: &str) -> Option<&Ordered> {
        match self {
            Ordered::Object(m) => m.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Ordered::String(s) => Some(s),
            _ => None,
        }
    }
}

/// Jackson's `String` binding: strings as is, `null` as null; anything else is coerced or
/// rejected depending on settings, so it is not decided here (`Err`).
fn string_or_null(v: Option<&Ordered>) -> Result<Option<&str>, ()> {
    match v {
        None | Some(Ordered::Null) => Ok(None),
        Some(Ordered::String(s)) => Ok(Some(s)),
        Some(_) => Err(()),
    }
}

fn write_opt(out: &mut String, s: Option<&str>) {
    match s {
        Some(s) => jackson_string(out, s),
        None => out.push_str("null"),
    }
}

/// A `Sharing` column (`jsbObjectSharing`) as a plain `ObjectMapper` writes the `Sharing` object:
/// `{"owner":..,"users":{..},"userGroups":{..},"public":..}`. `None` when Java's reading would
/// fail or is not reproduced.
pub fn sharing_json(column: &str) -> Option<String> {
    let v = Ordered::parse(column)?;
    // JsonBinaryType's mapper ignores unknown members (measured: e.g. a stored "external")
    if !matches!(v, Ordered::Object(_)) {
        return None;
    }
    let owner = string_or_null(v.get("owner")).ok()?;
    let public = string_or_null(v.get("public")).ok()?;
    let mut out = String::from("{\"owner\":");
    write_opt(&mut out, owner);
    for (name, key) in [("users", "users"), ("userGroups", "userGroups")] {
        out.push_str(",\"");
        out.push_str(name);
        out.push_str("\":{");
        match v.get(key) {
            None | Some(Ordered::Null) => {}
            Some(Ordered::Object(entries)) => {
                for (i, (k, a)) in entries.iter().enumerate() {
                    if !matches!(a, Ordered::Object(_)) {
                        return None;
                    }
                    if i > 0 {
                        out.push(',');
                    }
                    jackson_string(&mut out, k);
                    // displayName is read-only for Jackson: never read from storage
                    out.push_str(":{\"displayName\":null,\"access\":");
                    write_opt(&mut out, string_or_null(a.get("access")).ok()?);
                    out.push_str(",\"id\":");
                    write_opt(&mut out, string_or_null(a.get("id")).ok()?);
                    out.push('}');
                }
            }
            Some(_) => return None,
        }
        out.push('}');
    }
    out.push_str(",\"public\":");
    write_opt(&mut out, public);
    out.push('}');
    Some(out)
}

/// `Objects.hashCode(String)`.
fn str_hash(s: Option<&str>) -> i32 {
    s.map_or(0, hash::string_hash_code)
}

/// `Locale#hashCode()` (a record: `31 * h + component` over language, region, script).
fn locale_hash(l: &Locale) -> i32 {
    let mut h = 0i32;
    for c in [
        Some(l.language.as_str()),
        l.region.as_deref(),
        l.script.as_deref(),
    ] {
        h = h.wrapping_mul(31).wrapping_add(str_hash(c));
    }
    h
}

/// One `Translation`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Translation {
    pub locale: Option<Locale>,
    pub property: Option<String>,
    pub value: Option<String>,
}

impl Translation {
    /// `Objects.hash(locale, property, value)`.
    fn java_hash(&self) -> i32 {
        let mut h = 1i32;
        for c in [
            self.locale.as_ref().map_or(0, locale_hash),
            str_hash(self.property.as_deref()),
            str_hash(self.value.as_deref()),
        ] {
            h = h.wrapping_mul(31).wrapping_add(c);
        }
        h
    }
}

/// A translations column (`jblTranslations`) read into a `HashSet<Translation>`, in its
/// iteration order. `None` when Java's reading fails (e.g. an invalid locale) or a bin would be
/// treeified.
pub fn translations(column: &str) -> Option<Vec<Translation>> {
    let Ordered::Array(items) = Ordered::parse(column)? else {
        return None;
    };
    let mut set: Vec<Translation> = Vec::new();
    for item in &items {
        let Ordered::Object(fields) = item else {
            return None;
        };
        if fields
            .iter()
            .any(|(f, _)| !["locale", "property", "value"].contains(&f.as_str()))
        {
            return None;
        }
        let locale = match string_or_null(item.get("locale")).ok()? {
            None => None,
            Some(l) => Some(Locale::parse(l).ok()?),
        };
        let t = Translation {
            locale,
            property: string_or_null(item.get("property"))
                .ok()?
                .map(str::to_string),
            value: string_or_null(item.get("value")).ok()?.map(str::to_string),
        };
        if !set.contains(&t) {
            set.push(t);
        }
    }
    let hashes: Vec<i32> = set.iter().map(Translation::java_hash).collect();
    let order = hash::hash_order(&hashes, Construction::Default)?;
    Some(order.into_iter().map(|i| set[i].clone()).collect())
}

/// Translations as a plain `ObjectMapper` writes them: a `Set` as an array of
/// `{"locale":..,"property":..,"value":..}`, or (JPA entities, `TranslationProperty`) wrapped
/// in `{"translations":[...]}`.
pub fn translations_json(column: &str, wrapped: bool) -> Option<String> {
    let set = translations(column)?;
    let mut out = String::new();
    if wrapped {
        out.push_str("{\"translations\":");
    }
    out.push('[');
    for (i, t) in set.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str("{\"locale\":");
        let locale = t.locale.as_ref().map(ToString::to_string);
        write_opt(&mut out, locale.as_deref());
        out.push_str(",\"property\":");
        write_opt(&mut out, t.property.as_deref());
        out.push_str(",\"value\":");
        write_opt(&mut out, t.value.as_deref());
        out.push('}');
    }
    out.push(']');
    if wrapped {
        out.push('}');
    }
    Some(out)
}

/// `getTranslation(key, default)` (`BaseIdentifiableObject`, `TranslationProperty`): the first
/// non-empty value for the locale and key (case-insensitive) in the set's iteration order. When
/// there are translations but none matches, the default comes back **trimmed**; without any,
/// unchanged. `None` when the column cannot be read as Java reads it.
pub fn translate(
    column: Option<&str>,
    locale: &Locale,
    key: &str,
    default: Option<&str>,
) -> Option<Option<String>> {
    let set = match column {
        None => Vec::new(),
        Some(c) => translations(c)?,
    };
    if set.is_empty() {
        return Some(default.map(str::to_string));
    }
    for t in &set {
        if t.locale.as_ref() == Some(locale)
            && t.property
                .as_deref()
                .is_some_and(|p| p.eq_ignore_ascii_case(key))
            && t.value.as_deref().is_some_and(|v| !v.is_empty())
        {
            return Some(t.value.clone());
        }
    }
    Some(default.map(|d| crate::java::string::trim(d).to_string()))
}

/// Attribute values (`jsbAttributeValues`): `(attribute uid, value)` in stored order, from the
/// object shape `{"uid": {"value": ".."}}`. `None` for shapes not reproduced here.
pub fn attribute_values(column: &str) -> Option<Vec<(String, String)>> {
    let Ordered::Object(entries) = Ordered::parse(column)? else {
        return None;
    };
    entries
        .iter()
        .map(|(k, v)| Some((k.clone(), v.get("value")?.as_str()?.to_string())))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Java's own reading and writing (`tools/gen-jsonb-fixtures.sh`): kind, column text,
    /// output (and for attribute values the json-tree object form, then the array form).
    #[test]
    fn values_like_java() {
        let fixtures = include_str!("../../dhis-query/tests/fixtures/jsonb-values.tsv");
        let mut n = 0;
        for line in fixtures.lines() {
            let cols: Vec<&str> = line.split('\t').collect();
            let (kind, input, want) = (cols[0], cols[1], cols[2]);
            let got = match kind {
                "sharing" => sharing_json(input),
                "translations" => translations_json(input, false),
                "translationProperty" => translations_json(input, true),
                "attributeValues" => attribute_values(input).map(|av| {
                    let mut o = String::from("{\"v\":{");
                    for (i, (k, v)) in av.iter().enumerate() {
                        if i > 0 {
                            o.push(',');
                        }
                        jackson_string(&mut o, k);
                        o.push(':');
                        jackson_string(&mut o, v);
                    }
                    o.push_str("}}");
                    o
                }),
                _ => unreachable!(),
            };
            if want.starts_with("ERROR") {
                assert_eq!(got, None, "{kind} {input}");
            } else {
                assert_eq!(got.as_deref(), Some(want), "{kind} {input}");
            }
            n += 1;
        }
        assert_eq!(n, 88);
    }
}
