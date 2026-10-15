//! Response normalisation rules.
//!
//! ```toml
//! # Headers compared in addition to the status code and body.
//! compare_headers = ["content-type", "content-disposition", "location"]
//!
//! [[ignore_key]]
//! key = "lastUpdated"
//! reason = "Set to now() on every write; differs between the two runs."
//!
//! [[ignore_key]]
//! key = "serverDate"
//! paths = ["/api/system/info"]   # optional: only for cases under one of these paths
//! reason = "Wall clock time of the server."
//!
//!
//! [[replace]]
//! regex = "[a-zA-Z][a-zA-Z0-9]{10}"
//! with = "<uid>"
//! paths = ["/api/system/uid", "/api/system/id"]
//! reason = "Freshly generated UIDs are random; the count and shape are what must match."
//!
//! [[unordered]]
//! key = "references"
//! paths = ["/api/schemas"]
//! reason = "A HashSet of Class: Java's own order changes on every restart."
//! ```
//!
//! `unordered` sorts every array found under that key (after `ignore_key`), so the arrays are
//! compared as multisets.
//!
//! `replace` rules run on the canonical text form (after JSON normalisation), so they apply to
//! JSON, CSV and XML alike.
//!
//! JSON bodies are compared structurally: object key order is ignored (it is not significant in
//! JSON), array order is kept. Non-JSON bodies are compared byte for byte.

use std::path::Path;

use anyhow::{Context, Result, bail};
use http::HeaderMap;
use serde::Deserialize;
use serde_json::Value;

use crate::corpus::Case;

/// Whether a rule's `paths` entry covers the case path: the same path, or that path followed by
/// a sub-path, query, extension or path parameter (`/api/me` covers `/api/me?fields=id` and
/// `/api/me/settings`, not `/api/metadata`). As in Java (`ApiVersionFilter`), an API version
/// 28..44 in the case path is ignored, so `/api/schemas` also covers `/api/41/schemas`.
fn path_matches(case_path: &str, rule_path: &str) -> bool {
    let versionless = strip_api_version(case_path);
    versionless
        .strip_prefix(rule_path)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(['/', '?', '.', ';']))
}

fn strip_api_version(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("/api/")
        && rest.len() > 2
        && rest.as_bytes()[2] == b'/'
        && rest[..2]
            .parse::<u32>()
            .is_ok_and(|v| (28..=44).contains(&v))
    {
        return format!("/api/{}", &rest[3..]);
    }
    path.to_string()
}

const DEFAULT_HEADERS: &[&str] = &["content-type", "content-disposition", "location"];

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rules {
    #[serde(default)]
    compare_headers: Option<Vec<String>>,
    #[serde(default)]
    ignore_key: Vec<IgnoreKey>,
    #[serde(default)]
    unordered: Vec<IgnoreKey>,
    #[serde(default)]
    replace: Vec<Replace>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Replace {
    #[serde(with = "serde_regex")]
    pub regex: regex::Regex,
    pub with: String,
    #[serde(default)]
    pub paths: Vec<String>,
    pub reason: String,
}

mod serde_regex {
    use serde::{Deserialize, Deserializer};

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<regex::Regex, D::Error> {
        let s = String::deserialize(d)?;
        regex::Regex::new(&s).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IgnoreKey {
    pub key: String,
    #[serde(default)]
    pub paths: Vec<String>,
    pub reason: String,
}

impl Rules {
    pub fn parse(text: &str) -> Result<Self> {
        let rules: Rules = toml::from_str(text)?;
        for (i, r) in rules.replace.iter().enumerate() {
            if r.reason.trim().len() < 10 {
                bail!(
                    "replace[{i}] ({}): every rule needs a real `reason`",
                    r.regex
                );
            }
        }
        for (i, r) in rules.unordered.iter().enumerate() {
            if r.reason.trim().len() < 10 {
                bail!(
                    "unordered[{i}] ({:?}): every rule needs a real `reason` explaining why the \
                     order is irrelevant",
                    r.key
                );
            }
        }
        for (i, r) in rules.ignore_key.iter().enumerate() {
            if r.reason.trim().len() < 10 {
                bail!(
                    "ignore_key[{i}] ({:?}): every rule needs a real `reason` explaining why the \
                     difference is irrelevant",
                    r.key
                );
            }
        }
        Ok(rules)
    }

    pub fn load(path: &Path) -> Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Self::parse(&text).with_context(|| path.display().to_string())
    }

    pub fn compared_headers(&self) -> Vec<String> {
        self.compare_headers
            .clone()
            .unwrap_or_else(|| DEFAULT_HEADERS.iter().map(|s| s.to_string()).collect())
    }

    /// A canonical text form of the body for comparison and diffing.
    pub fn normalize_body(&self, case: &Case, headers: &HeaderMap, body: &[u8]) -> String {
        let mut text = self.canonical(case, headers, body);
        for r in &self.replace {
            if r.paths.is_empty() || r.paths.iter().any(|p| path_matches(&case.path, p)) {
                text = r.regex.replace_all(&text, r.with.as_str()).into_owned();
            }
        }
        text
    }

    fn canonical(&self, case: &Case, headers: &HeaderMap, body: &[u8]) -> String {
        let is_json = headers
            .get(http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|ct| ct.contains("json"));
        if is_json && let Ok(mut v) = serde_json::from_slice::<Value>(body) {
            let keys: Vec<&str> = self
                .ignore_key
                .iter()
                .filter(|r| {
                    r.paths.is_empty() || r.paths.iter().any(|p| path_matches(&case.path, p))
                })
                .map(|r| r.key.as_str())
                .collect();
            strip_keys(&mut v, &keys);
            let unordered: Vec<&str> = self
                .unordered
                .iter()
                .filter(|r| {
                    r.paths.is_empty() || r.paths.iter().any(|p| path_matches(&case.path, p))
                })
                .map(|r| r.key.as_str())
                .collect();
            sort_arrays(&mut v, &unordered);
            // serde_json's default Map is a BTreeMap, so this prints keys sorted.
            return serde_json::to_string_pretty(&v).unwrap_or_default();
        }
        String::from_utf8_lossy(body).into_owned()
    }
}

/// Sorts (by canonical text) every array that is the value of one of `keys`, innermost first.
fn sort_arrays(v: &mut Value, keys: &[&str]) {
    match v {
        Value::Object(map) => {
            for (k, child) in map.iter_mut() {
                sort_arrays(child, keys);
                if keys.contains(&k.as_str())
                    && let Value::Array(items) = child
                {
                    items.sort_by_cached_key(|i| serde_json::to_string(i).unwrap_or_default());
                }
            }
        }
        Value::Array(items) => {
            for child in items {
                sort_arrays(child, keys);
            }
        }
        _ => {}
    }
}

fn strip_keys(v: &mut Value, keys: &[&str]) {
    match v {
        Value::Object(map) => {
            for k in keys {
                map.remove(*k);
            }
            for child in map.values_mut() {
                strip_keys(child, keys);
            }
        }
        Value::Array(items) => {
            for child in items {
                strip_keys(child, keys);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::HeaderValue;

    fn json_headers() -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(
            "content-type",
            HeaderValue::from_static("application/json;charset=UTF-8"),
        );
        h
    }

    fn case(path: &str) -> Case {
        Case {
            name: "t".into(),
            method: "GET".into(),
            path: path.into(),
            headers: Default::default(),
            body: None,
            tags: vec![],
        }
    }

    #[test]
    fn rules_require_reasons() {
        assert!(Rules::parse("[[ignore_key]]\nkey = \"x\"\nreason = \"\"").is_err());
        assert!(Rules::parse("[[ignore_key]]\nkey = \"x\"").is_err());
        assert!(
            Rules::parse("[[ignore_key]]\nkey = \"x\"\nreason = \"wall clock timestamp\"").is_ok()
        );
    }

    #[test]
    fn key_order_is_ignored_array_order_is_not() {
        let r = Rules::default();
        let c = case("/api/x");
        let a = r.normalize_body(&c, &json_headers(), br#"{"a":1,"b":[1,2]}"#);
        let b = r.normalize_body(&c, &json_headers(), br#"{"b":[1,2],"a":1}"#);
        let d = r.normalize_body(&c, &json_headers(), br#"{"b":[2,1],"a":1}"#);
        assert_eq!(a, b);
        assert_ne!(a, d);
    }

    #[test]
    fn ignored_keys_are_scoped_by_path() {
        let r = Rules::parse(
            "[[ignore_key]]\nkey = \"serverDate\"\npaths = [\"/api/system/info\"]\n\
             reason = \"wall clock time of the server\"",
        )
        .unwrap();
        let body = br#"{"serverDate":"2026-01-01","nested":{"serverDate":"x"},"v":1}"#;
        let n = r.normalize_body(&case("/api/system/info"), &json_headers(), body);
        assert!(!n.contains("serverDate"));
        let n = r.normalize_body(&case("/api/me"), &json_headers(), body);
        assert!(n.contains("serverDate"));
    }

    #[test]
    fn unordered_arrays_compare_as_multisets() {
        let r = Rules::parse(
            "[[unordered]]\nkey = \"refs\"\npaths = [\"/api/schemas\"]\n\
             reason = \"set order changes per JVM start\"",
        )
        .unwrap();
        let c = case("/api/schemas/x");
        let a = r.normalize_body(&c, &json_headers(), br#"{"refs":["b","a"],"o":[2,1]}"#);
        let b = r.normalize_body(&c, &json_headers(), br#"{"refs":["a","b"],"o":[2,1]}"#);
        let d = r.normalize_body(&c, &json_headers(), br#"{"refs":["a","b"],"o":[1,2]}"#);
        assert_eq!(a, b);
        assert_ne!(a, d);
        assert!(Rules::parse("[[unordered]]\nkey = \"x\"\nreason = \"\"").is_err());
    }

    #[test]
    fn paths_match_whole_segments() {
        assert!(path_matches("/api/me", "/api/me"));
        assert!(path_matches("/api/me?fields=id", "/api/me"));
        assert!(path_matches("/api/me/settings", "/api/me"));
        assert!(path_matches("/api/me.json", "/api/me"));
        assert!(!path_matches("/api/metadata", "/api/me"));
        assert!(path_matches("/api/41/schemas?fields=name", "/api/schemas"));
        assert!(!path_matches("/api/45/schemas", "/api/schemas"));
    }

    #[test]
    fn replace_rules() {
        let r = Rules::parse(
            "[[replace]]\nregex = \"[a-zA-Z][a-zA-Z0-9]{10}\"\nwith = \"<uid>\"\n\
             reason = \"generated UIDs are random\"",
        )
        .unwrap();
        let a = r.normalize_body(
            &case("/a"),
            &json_headers(),
            br#"{"codes":["abcdefghijk"]}"#,
        );
        let b = r.normalize_body(
            &case("/a"),
            &json_headers(),
            br#"{"codes":["Zyxwvutsrq1"]}"#,
        );
        assert_eq!(a, b);
        assert!(Rules::parse("[[replace]]\nregex = \"x\"\nwith = \"y\"\nreason = \"\"").is_err());
    }

    #[test]
    fn non_json_is_compared_verbatim() {
        let r = Rules::default();
        let mut h = HeaderMap::new();
        h.insert("content-type", HeaderValue::from_static("text/csv"));
        assert_eq!(r.normalize_body(&case("/a"), &h, b"a,b\n"), "a,b\n");
    }
}
