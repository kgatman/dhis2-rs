//! Phase-0 inventory generator.
//!
//! Parses the Java dhis2-core source (read-only) and writes the progress ledger:
//!
//! - `docs/inventory/endpoints.csv`  — every Spring MVC mapping in `dhis-web-api`
//! - `docs/inventory/tables.csv`     — ORM-mapped (hbm.xml + JPA) and Flyway-created tables
//! - `docs/inventory/config-keys.csv`— every `ConfigurationKey`
//! - `docs/inventory/jobs.csv`       — every `JobType`
//! - `crates/dhis-config/src/keys_generated.rs`
//!
//! The `status` columns (`java` -> `rust-shadow` -> `rust`) are preserved across regenerations.
//! `--check` fails if any output would change (used in CI to catch drift).

mod config_keys;
mod endpoints;
mod eval;
mod java;
mod jobs;
mod tables;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Parser;

#[derive(Parser)]
#[command(about = "Generate docs/inventory from the Java dhis2-core source")]
struct Args {
    /// Root of the dhis2-core checkout (contains `dhis-2/`).
    #[arg(long, default_value = "../dhis2-core")]
    java_root: PathBuf,
    /// Root of this repository.
    #[arg(long, default_value = ".")]
    repo_root: PathBuf,
    /// Do not write; exit non-zero if any output would change.
    #[arg(long)]
    check: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let dhis2 = args.java_root.join("dhis-2");
    if !dhis2.is_dir() {
        bail!("{} is not a dhis2-core checkout", args.java_root.display());
    }
    let out_dir = args.repo_root.join("docs/inventory");
    let keys_rs = args
        .repo_root
        .join("crates/dhis-config/src/keys_generated.rs");

    // Parse every main Java source once.
    let mut idx = java::JavaIndex::default();
    let files = java::main_java_files(&dhis2);
    for f in &files {
        let src = std::fs::read_to_string(f).with_context(|| format!("reading {}", f.display()))?;
        idx.add_file(f, &src)?;
    }
    eprintln!(
        "parsed {} Java files, {} types",
        files.len(),
        idx.types.len()
    );

    let mut outputs: Vec<(PathBuf, String)> = Vec::new();

    // Endpoints.
    let ep_path = out_dir.join("endpoints.csv");
    let statuses = endpoints::existing_status(&ep_path);
    let report = endpoints::extract(&idx, &dhis2);
    for w in &report.warnings {
        eprintln!("warning: {w}");
    }
    let rows: Vec<Vec<String>> = report
        .endpoints
        .iter()
        .map(|e| {
            let s = statuses.get(&e.key()).map(String::as_str).unwrap_or("java");
            e.record(s)
        })
        .collect();
    outputs.push((ep_path, to_csv(endpoints::COLUMNS, &rows)?));

    // Config keys.
    let keys = config_keys::extract(&idx)?;
    let rows: Vec<Vec<String>> = keys.iter().map(config_keys::record).collect();
    outputs.push((
        out_dir.join("config-keys.csv"),
        to_csv(config_keys::COLUMNS, &rows)?,
    ));
    outputs.push((keys_rs, config_keys::render_rust(&keys)));

    // Jobs.
    let jobs_path = out_dir.join("jobs.csv");
    let job_status = column_by_key(&jobs_path, "name", "status");
    let rows: Vec<Vec<String>> = jobs::extract(&idx, &dhis2)?
        .into_iter()
        .map(|mut r| {
            let status = job_status
                .get(&r["name"])
                .cloned()
                .unwrap_or_else(|| "java".into());
            r.insert("status", status);
            jobs::COLUMNS
                .iter()
                .map(|c| r.get(c).cloned().unwrap_or_default())
                .collect()
        })
        .collect();
    outputs.push((jobs_path, to_csv(jobs::COLUMNS, &rows)?));

    // Tables.
    let rows = tables::extract(&dhis2, &idx)?;
    outputs.push((out_dir.join("tables.csv"), to_csv(tables::COLUMNS, &rows)?));

    let mut drift = Vec::new();
    for (path, content) in &outputs {
        let current = std::fs::read_to_string(path).unwrap_or_default();
        if current == *content {
            continue;
        }
        if args.check {
            drift.push(path.display().to_string());
        } else {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(path, content)?;
            eprintln!("wrote {}", path.display());
        }
    }
    if !drift.is_empty() {
        bail!("inventory out of date: {}", drift.join(", "));
    }

    summary(&report.endpoints, keys.len(), rows.len());
    Ok(())
}

fn to_csv(columns: &[&str], rows: &[Vec<String>]) -> Result<String> {
    let mut w = csv::WriterBuilder::new()
        .terminator(csv::Terminator::Any(b'\n'))
        .from_writer(Vec::new());
    w.write_record(columns)?;
    for r in rows {
        w.write_record(r)?;
    }
    Ok(String::from_utf8(w.into_inner()?)?)
}

fn column_by_key(path: &Path, key: &str, value: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Ok(mut rdr) = csv::Reader::from_path(path) else {
        return out;
    };
    let Ok(headers) = rdr.headers().cloned() else {
        return out;
    };
    let (Some(ki), Some(vi)) = (
        headers.iter().position(|h| h == key),
        headers.iter().position(|h| h == value),
    ) else {
        return out;
    };
    for r in rdr.records().flatten() {
        if let (Some(k), Some(v)) = (r.get(ki), r.get(vi)) {
            out.insert(k.to_string(), v.to_string());
        }
    }
    out
}

fn summary(endpoints: &[endpoints::Endpoint], keys: usize, tables: usize) {
    let controllers: std::collections::BTreeSet<&str> =
        endpoints.iter().map(|e| e.controller.as_str()).collect();
    eprintln!(
        "endpoints: {} rows across {} controllers; config keys: {keys}; tables: {tables}",
        endpoints.len(),
        controllers.len()
    );
}
