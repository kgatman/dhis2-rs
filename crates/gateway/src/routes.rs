//! The route table: which backend serves a request.
//!
//! Anything not matched by a rule goes to Java. Rules are tried in file order; the first match
//! wins. Paths are matched after the same normalisation Java applies before handler lookup, so a
//! rule written for `/api/schemas` also covers `/api/41/schemas`, `/api/schemas/` and
//! `/api/schemas.json`:
//!
//! - `org.hisp.dhis.webapi.filter.ApiVersionFilter`: `/api/{28..44}/x` -> `/api/x`;
//! - `org.hisp.dhis.webapi.mvc.CustomRequestMappingHandlerMapping`: one trailing `/` and a
//!   registered media-type extension (`WebMvcConfig.MEDIA_TYPE_MAP`) are stripped.
//!
//! Pattern syntax: literal segments, `*` (exactly one segment), `{name}` (one segment) and a final
//! `**` (zero or more segments).

use std::path::Path;
use std::sync::LazyLock;

use http::Method;
use regex::Regex;
use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    Java,
    Rust,
    /// Java serves the client; the same request is replayed to Rust in the background and the
    /// responses are compared. Only allowed for GET/HEAD: replaying a write would apply it twice
    /// to the shared database.
    Shadow,
}

#[derive(Debug, Error)]
pub enum RouteError {
    #[error("reading {0}: {1}")]
    Io(String, std::io::Error),
    #[error("parsing {0}: {1}")]
    Parse(String, toml::de::Error),
    #[error("rule {index}: {message}")]
    Invalid { index: usize, message: String },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RouteFile {
    #[serde(default, rename = "route")]
    routes: Vec<RawRule>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRule {
    /// HTTP methods; empty means any.
    #[serde(default)]
    methods: Vec<String>,
    path: String,
    backend: Backend,
    /// Free text: which Java controller this covers, ticket, etc.
    #[serde(default)]
    #[allow(dead_code)]
    note: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Rule {
    methods: Vec<Method>,
    segments: Vec<Seg>,
    pub pattern: String,
    pub backend: Backend,
}

#[derive(Debug, Clone, PartialEq)]
enum Seg {
    Literal(String),
    One,
    Rest,
}

#[derive(Debug, Clone, Default)]
pub struct RouteTable {
    rules: Vec<Rule>,
}

/// `WebMvcConfig.MEDIA_TYPE_MAP` keys.
const MEDIA_TYPE_EXTENSIONS: &[&str] = &[
    "json",
    "json.gz",
    "json.zip",
    "jsonp",
    "xml",
    "xml.gz",
    "xml.zip",
    "png",
    "pdf",
    "xls",
    "xlsx",
    "csv",
    "csv.gz",
    "csv.zip",
    "adx.xml",
    "adx.xml.gz",
    "adx.xml.zip",
    "geojson",
];

/// `ApiVersionFilter.API_VERSION_REGEX`.
static API_VERSION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(/api/)(2[8-9]|3\d|4[0-4])/(.*)$").expect("valid regex"));

impl RouteTable {
    /// Everything to Java.
    pub fn all_java() -> Self {
        Self::default()
    }

    pub fn load(path: &Path) -> Result<Self, RouteError> {
        let name = path.display().to_string();
        let text = std::fs::read_to_string(path).map_err(|e| RouteError::Io(name.clone(), e))?;
        Self::parse(&text).map_err(|e| match e {
            RouteError::Parse(_, err) => RouteError::Parse(name, err),
            other => other,
        })
    }

    pub fn parse(text: &str) -> Result<Self, RouteError> {
        let file: RouteFile =
            toml::from_str(text).map_err(|e| RouteError::Parse("<string>".into(), e))?;
        let mut rules = Vec::new();
        for (index, raw) in file.routes.into_iter().enumerate() {
            let invalid = |message: String| RouteError::Invalid { index, message };
            if !raw.path.starts_with('/') {
                return Err(invalid(format!("path {:?} must start with '/'", raw.path)));
            }
            let mut segments = Vec::new();
            let parts: Vec<&str> = raw.path[1..].split('/').collect();
            for (i, p) in parts.iter().enumerate() {
                let seg = match *p {
                    "**" if i + 1 == parts.len() => Seg::Rest,
                    "**" => return Err(invalid("'**' is only allowed at the end".into())),
                    "*" => Seg::One,
                    p if p.starts_with('{') && p.ends_with('}') => Seg::One,
                    "" if parts.len() == 1 => continue,
                    "" => return Err(invalid("empty path segment".into())),
                    p => Seg::Literal(p.to_string()),
                };
                segments.push(seg);
            }
            let methods: Vec<Method> = raw
                .methods
                .iter()
                .map(|m| {
                    Method::from_bytes(m.to_ascii_uppercase().as_bytes())
                        .map_err(|_| invalid(format!("bad method {m:?}")))
                })
                .collect::<Result<_, _>>()?;
            if raw.backend == Backend::Shadow
                && (methods.is_empty()
                    || methods
                        .iter()
                        .any(|m| *m != Method::GET && *m != Method::HEAD))
            {
                return Err(invalid(
                    "shadow rules must list methods, and only GET/HEAD (writes would run twice)"
                        .into(),
                ));
            }
            rules.push(Rule {
                methods,
                segments,
                pattern: raw.path,
                backend: raw.backend,
            });
        }
        Ok(Self { rules })
    }

    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }

    /// The backend for a request path (no query string).
    pub fn route(&self, method: &Method, path: &str) -> (Backend, Option<&Rule>) {
        if self.rules.is_empty() {
            return (Backend::Java, None);
        }
        let candidates = candidate_paths(path);
        for rule in &self.rules {
            if !rule.methods.is_empty() && !rule.methods.contains(method) {
                continue;
            }
            if candidates.iter().any(|p| rule.matches(p)) {
                return (rule.backend, Some(rule));
            }
        }
        (Backend::Java, None)
    }
}

impl Rule {
    fn matches(&self, path: &str) -> bool {
        let parts: Vec<&str> = path
            .strip_prefix('/')
            .unwrap_or(path)
            .split('/')
            .filter(|s| !s.is_empty())
            .collect();
        let mut i = 0;
        for seg in &self.segments {
            match seg {
                Seg::Rest => return true,
                Seg::One => {
                    if i >= parts.len() {
                        return false;
                    }
                }
                Seg::Literal(l) => {
                    if parts.get(i) != Some(&l.as_str()) {
                        return false;
                    }
                }
            }
            i += 1;
        }
        i == parts.len()
    }
}

/// The path a handler sees after Java's normalisation (version prefix, one trailing slash and a
/// registered media-type extension stripped), plus the stripped extension.
pub fn normalize(path: &str) -> (String, Option<String>) {
    let unversioned = match API_VERSION.captures(path) {
        Some(c) => format!("{}{}", &c[1], &c[3]),
        None => path.to_string(),
    };
    let mut p = unversioned.as_str();
    if p.len() > 1 && p.ends_with('/') {
        p = &p[..p.len() - 1];
    }
    let last_sep = p.rfind('/').map_or(0, |i| i + 1);
    if let Some(dot) = p[last_sep..].find('.') {
        let ext = &p[last_sep + dot + 1..];
        if MEDIA_TYPE_EXTENSIONS.contains(&ext) {
            return (p[..last_sep + dot].to_string(), Some(ext.to_string()));
        }
    }
    (p.to_string(), None)
}

/// The version-stripped path, and its suffix/slash-normalised form when different.
pub fn candidate_paths(path: &str) -> Vec<String> {
    let unversioned = match API_VERSION.captures(path) {
        Some(c) => format!("{}{}", &c[1], &c[3]),
        None => path.to_string(),
    };
    let mut out = vec![unversioned.clone()];
    let mut p = unversioned.as_str();
    if p.len() > 1 && p.ends_with('/') {
        p = &p[..p.len() - 1];
    }
    let last_sep = p.rfind('/').map_or(0, |i| i + 1);
    if let Some(dot) = p[last_sep..].find('.') {
        let ext = &p[last_sep + dot + 1..];
        if MEDIA_TYPE_EXTENSIONS.contains(&ext) {
            p = &p[..last_sep + dot];
        }
    }
    if p != unversioned {
        out.push(p.to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const TABLE: &str = r#"
[[route]]
methods = ["GET"]
path = "/api/schemas/**"
backend = "rust"

[[route]]
path = "/api/system/info"
backend = "rust"

[[route]]
methods = ["GET"]
path = "/api/dataSets/{uid}"
backend = "rust"

[[route]]
path = "/api/dataSets/**"
backend = "java"
"#;

    fn route(t: &RouteTable, m: Method, p: &str) -> Backend {
        t.route(&m, p).0
    }

    #[test]
    fn empty_table_is_all_java() {
        let t = RouteTable::all_java();
        assert_eq!(route(&t, Method::GET, "/api/schemas"), Backend::Java);
    }

    #[test]
    fn matching_and_order() {
        let t = RouteTable::parse(TABLE).unwrap();
        assert_eq!(route(&t, Method::GET, "/api/schemas"), Backend::Rust);
        assert_eq!(
            route(&t, Method::GET, "/api/schemas/dataElement"),
            Backend::Rust
        );
        assert_eq!(
            route(&t, Method::POST, "/api/schemas/dataElement"),
            Backend::Java
        );
        assert_eq!(route(&t, Method::PUT, "/api/system/info"), Backend::Rust);
        assert_eq!(route(&t, Method::GET, "/api/system/infox"), Backend::Java);
        assert_eq!(
            route(&t, Method::GET, "/api/dataSets/abcdefghijk"),
            Backend::Rust
        );
        assert_eq!(
            route(&t, Method::DELETE, "/api/dataSets/abcdefghijk"),
            Backend::Java
        );
        assert_eq!(
            route(&t, Method::GET, "/api/dataSets/abc/form"),
            Backend::Java
        );
        assert_eq!(route(&t, Method::GET, "/api/dataSets"), Backend::Java);
    }

    #[test]
    fn java_normalisation_applies() {
        let t = RouteTable::parse(TABLE).unwrap();
        for p in [
            "/api/41/system/info",
            "/api/28/system/info",
            "/api/44/system/info",
            "/api/system/info/",
            "/api/system/info.json",
            "/api/system/info.xml/",
            "/api/42/system/info.json",
        ] {
            assert_eq!(route(&t, Method::GET, p), Backend::Rust, "{p}");
        }
        for p in [
            "/api/27/system/info",
            "/api/45/system/info",
            "/api/333/system/info",
            "/api/system/info.txt",
        ] {
            assert_eq!(route(&t, Method::GET, p), Backend::Java, "{p}");
        }
    }

    #[test]
    fn candidate_paths_like_java() {
        assert_eq!(candidate_paths("/api/40/me"), vec!["/api/me"]);
        assert_eq!(
            candidate_paths("/api/dataValueSets.adx.xml.gz"),
            vec!["/api/dataValueSets.adx.xml.gz", "/api/dataValueSets"]
        );
        assert_eq!(candidate_paths("/"), vec!["/"]);
    }

    #[test]
    fn normalize_like_java() {
        assert_eq!(
            normalize("/api/41/system/uid.json/"),
            ("/api/system/uid".into(), Some("json".into()))
        );
        assert_eq!(
            normalize("/api/x.adx.xml.gz"),
            ("/api/x".into(), Some("adx.xml.gz".into()))
        );
        assert_eq!(normalize("/api/x.txt"), ("/api/x.txt".into(), None));
        assert_eq!(normalize("/"), ("/".into(), None));
    }

    #[test]
    fn invalid_tables_are_rejected() {
        assert!(RouteTable::parse("[[route]]\npath = \"api\"\nbackend = \"rust\"").is_err());
        assert!(RouteTable::parse("[[route]]\npath = \"/a/**/b\"\nbackend = \"rust\"").is_err());
        assert!(RouteTable::parse("[[route]]\npath = \"/a\"\nbackend = \"go\"").is_err());
        assert!(RouteTable::parse("[[route]]\npath = \"/a\"\nbackend = \"shadow\"").is_err());
        assert!(
            RouteTable::parse(
                "[[route]]\nmethods = [\"GET\", \"POST\"]\npath = \"/a\"\nbackend = \"shadow\""
            )
            .is_err()
        );
        assert!(
            RouteTable::parse(
                "[[route]]\nmethods = [\"GET\"]\npath = \"/a\"\nbackend = \"shadow\""
            )
            .is_ok()
        );
        assert!(RouteTable::parse("[[route]]\npath = \"/a\"\nbackend = \"rust\"\nx = 1").is_err());
    }
}
