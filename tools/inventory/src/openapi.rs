//! Cross-checks of the generated inventory against a live Java instance: `endpoints.csv` vs
//! `/api/openapi.json`, and `tables.csv` vs the tables of a Java-migrated database.
//!
//! Both sides are reduced to `METHOD /path` with path variables anonymised (`{uid:[a-z]{11}}`
//! and `{id}` both become `{}`), then compared as sets.

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context, Result};

/// `/api/x/{uid:[a-zA-Z0-9]{11}}/{p}` -> `/api/x/{}/{}`.
pub fn anonymise(path: &str) -> String {
    let mut out = String::new();
    let mut depth = 0usize;
    for c in path.chars() {
        match c {
            '{' => {
                if depth == 0 {
                    out.push_str("{}");
                }
                depth += 1;
            }
            '}' => depth = depth.saturating_sub(1),
            _ if depth > 0 => {}
            c => out.push(c),
        }
    }
    out
}

fn from_csv(path: &Path) -> Result<BTreeSet<String>> {
    let mut rdr = csv::Reader::from_path(path).with_context(|| path.display().to_string())?;
    let mut out = BTreeSet::new();
    for r in rdr.records() {
        let r = r?;
        out.insert(format!("{} {}", &r[1], anonymise(&r[2])));
    }
    Ok(out)
}

fn from_openapi(path: &Path) -> Result<BTreeSet<String>> {
    let text = std::fs::read_to_string(path).with_context(|| path.display().to_string())?;
    let spec: serde_json::Value = serde_json::from_str(&text)?;
    let mut out = BTreeSet::new();
    let paths = spec["paths"]
        .as_object()
        .context("no `paths` in OpenAPI document")?;
    for (p, ops) in paths {
        for m in ["get", "post", "put", "delete", "patch", "head", "options"] {
            if ops.get(m).is_some() {
                out.insert(format!("{} {}", m.to_uppercase(), anonymise(p)));
            }
        }
    }
    Ok(out)
}

pub fn cross_check(spec: &Path, csv: &Path) -> Result<()> {
    let ours = from_csv(csv)?;
    let theirs = from_openapi(spec)?;
    let only_csv: Vec<_> = ours.difference(&theirs).collect();
    let only_api: Vec<_> = theirs.difference(&ours).collect();
    println!(
        "endpoints.csv: {} | openapi: {} | common: {}",
        ours.len(),
        theirs.len(),
        ours.intersection(&theirs).count()
    );
    println!("\n## Only in endpoints.csv ({})", only_csv.len());
    for e in &only_csv {
        println!("{e}");
    }
    println!("\n## Only in openapi.json ({})", only_api.len());
    for e in &only_api {
        println!("{e}");
    }
    Ok(())
}

pub fn cross_check_tables(list: &Path, csv: &Path) -> Result<()> {
    let live: BTreeSet<String> = std::fs::read_to_string(list)
        .with_context(|| list.display().to_string())?
        .lines()
        .map(|l| l.trim().to_lowercase())
        .filter(|l| !l.is_empty())
        .collect();
    let mut rdr = csv::Reader::from_path(csv)?;
    let mut present = BTreeSet::new();
    let mut conditional = BTreeSet::new();
    for r in rdr.records() {
        let r = r?;
        match &r[1] {
            "true" => {
                present.insert(r[0].to_string());
            }
            "conditional" => {
                conditional.insert(r[0].to_string());
            }
            _ => {}
        }
    }
    let only_csv: Vec<_> = present.difference(&live).collect();
    // Conditionally created tables may or may not exist, depending on the data migrated.
    let only_db: Vec<_> = live
        .difference(&present)
        .filter(|t| !conditional.contains(*t))
        .collect();
    println!(
        "tables.csv present: {} | database: {} | common: {}",
        present.len(),
        live.len(),
        present.intersection(&live).count()
    );
    println!(
        "\n## Present per tables.csv but not in the database ({})",
        only_csv.len()
    );
    for t in &only_csv {
        println!("{t}");
    }
    println!(
        "\n## In the database but not present per tables.csv ({})",
        only_db.len()
    );
    for t in &only_db {
        println!("{t}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::anonymise;

    #[test]
    fn anonymises_variables() {
        assert_eq!(
            anonymise("/api/x/{uid:[a-zA-Z0-9]{11}}/{p}"),
            "/api/x/{}/{}"
        );
        assert_eq!(anonymise("/api/x"), "/api/x");
    }
}
