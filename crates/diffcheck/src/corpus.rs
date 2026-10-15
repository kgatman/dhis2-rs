//! Request corpus: one JSON object per line.
//!
//! ```json
//! {"name": "schemas list", "method": "GET", "path": "/api/schemas?fields=name",
//!  "headers": {"Accept": "application/json"}, "body": null, "tags": ["SchemaController"]}
//! ```
//!
//! Lines that are blank or start with `#` or `//` are skipped.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Case {
    pub name: String,
    #[serde(default = "get")]
    pub method: String,
    /// Path and query, starting with `/`.
    pub path: String,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub body: Option<String>,
    /// Free-form labels (controller name, "negative", ...), usable with `--tag`.
    #[serde(default)]
    pub tags: Vec<String>,
}

fn get() -> String {
    "GET".into()
}

pub fn parse(text: &str) -> Result<Vec<Case>> {
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let l = line.trim();
        if l.is_empty() || l.starts_with('#') || l.starts_with("//") {
            continue;
        }
        let case: Case =
            serde_json::from_str(l).with_context(|| format!("corpus line {}", i + 1))?;
        if !case.path.starts_with('/') {
            bail!("corpus line {}: path must start with '/'", i + 1);
        }
        out.push(case);
    }
    Ok(out)
}

pub fn load(path: &Path) -> Result<Vec<Case>> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    parse(&text).with_context(|| path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lines() {
        let cases = parse(
            "# comment\n\n{\"name\":\"a\",\"path\":\"/api/me\"}\n\
             {\"name\":\"b\",\"method\":\"POST\",\"path\":\"/api/x\",\"body\":\"{}\",\"tags\":[\"t\"]}\n",
        )
        .unwrap();
        assert_eq!(cases.len(), 2);
        assert_eq!(cases[0].method, "GET");
        assert_eq!(cases[1].body.as_deref(), Some("{}"));
        assert!(parse("{\"name\":\"a\",\"path\":\"api\"}").is_err());
        assert!(parse("{\"name\":\"a\",\"path\":\"/a\",\"bogus\":1}").is_err());
    }
}
