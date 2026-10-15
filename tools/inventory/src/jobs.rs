//! `JobType.java` (+ `Job` implementations) -> `docs/inventory/jobs.csv`.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, bail};
use regex::Regex;

use crate::java::{Expr, JavaIndex};

pub const FQN: &str = "org.hisp.dhis.scheduling.JobType";

pub const COLUMNS: &[&str] = &[
    "status",
    "name",
    "ordinal",
    "user_defined",
    "parameters_class",
    "default_uid",
    "default_name",
    "default_cron",
    "default_delay_seconds",
    "valid_user_required",
    "using_notifications",
    "using_error_notification",
    "using_continuous_execution",
    "relative_api_elements",
    "implementations",
];

pub fn extract(idx: &JavaIndex, java_root: &Path) -> Result<Vec<BTreeMap<&'static str, String>>> {
    let t = idx.by_fqn(FQN).context("JobType.java not found")?;
    let this_eq = Regex::new(r"this\s*==\s*([A-Z][A-Z0-9_]*)")?;
    let ordinal_lt = Regex::new(r"ordinal\(\)\s*<\s*([A-Z][A-Z0-9_]*)\.ordinal\(\)")?;
    let case_map = Regex::new(r"case\s+([A-Z0-9_,\s]+?)\s*->\s*Map\.of\(([^;]*?)\);")?;
    let str_lit = Regex::new(r#""([^"]*)""#)?;

    let flag_sets: BTreeMap<&str, Vec<String>> = [
        "isValidUserRequiredForJob",
        "isUsingNotifications",
        "isUsingErrorNotification",
        "isUsingContinuousExecution",
    ]
    .into_iter()
    .map(|m| {
        let body = t
            .methods
            .iter()
            .find(|x| x.name == m)
            .and_then(|x| x.body.clone())
            .unwrap_or_default();
        (
            m,
            this_eq
                .captures_iter(&body)
                .map(|c| c[1].to_string())
                .collect(),
        )
    })
    .collect();
    if flag_sets.values().all(Vec::is_empty) {
        bail!("JobType flag methods not found; parser is probably broken");
    }

    let user_defined_bound = t
        .methods
        .iter()
        .find(|m| m.name == "isUserDefined")
        .and_then(|m| m.body.as_deref())
        .and_then(|b| ordinal_lt.captures(b).map(|c| c[1].to_string()))
        .context("isUserDefined bound not found")?;
    let bound_ordinal = t
        .enum_constants
        .iter()
        .position(|c| c.name == user_defined_bound)
        .context("isUserDefined bound constant missing")?;

    let mut relative: BTreeMap<String, String> = BTreeMap::new();
    if let Some(body) = t
        .methods
        .iter()
        .find(|m| m.name == "getRelativeApiElements")
        .and_then(|m| m.body.as_deref())
    {
        for c in case_map.captures_iter(body) {
            let lits: Vec<String> = str_lit
                .captures_iter(&c[2])
                .map(|l| l[1].to_string())
                .collect();
            let pairs: Vec<String> = lits
                .chunks(2)
                .map(|p| format!("{}={}", p[0], p.get(1).cloned().unwrap_or_default()))
                .collect();
            for name in c[1].split(',') {
                relative.insert(name.trim().to_string(), pairs.join(";"));
            }
        }
    }

    // Job implementations: types with `getJobType()` returning a JobType constant.
    let names: Vec<&str> = t.enum_constants.iter().map(|c| c.name.as_str()).collect();
    let mut impls: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for ty in &idx.types {
        for m in ty
            .methods
            .iter()
            .filter(|m| m.name == "getJobType" && m.params.is_empty())
        {
            for r in &m.returns {
                if let Expr::Name(parts) = r
                    && let Some(last) = parts.last()
                    && names.contains(&last.as_str())
                {
                    let rel = ty.file.strip_prefix(java_root).unwrap_or(&ty.file);
                    let _ = rel;
                    impls
                        .entry(last.clone())
                        .or_default()
                        .push(ty.simple_name.clone());
                }
            }
        }
    }

    let mut rows = Vec::new();
    for (ordinal, c) in t.enum_constants.iter().enumerate() {
        let mut row: BTreeMap<&'static str, String> = BTreeMap::new();
        row.insert("name", c.name.clone());
        row.insert("ordinal", ordinal.to_string());
        row.insert("user_defined", (ordinal < bound_ordinal).to_string());
        let mut params = String::new();
        match c.args.as_slice() {
            [] => {}
            [Expr::ClassLit(cls)] => params = cls.clone(),
            [Expr::Call { name, args, .. }] => {
                let s = |i: usize| match args.get(i) {
                    Some(Expr::Str(v)) => v.clone(),
                    Some(Expr::Int(v)) => v.to_string(),
                    _ => String::new(),
                };
                let (uid, dname, cron, delay) = match name.as_str() {
                    "every" => (s(1), s(2), String::new(), s(0)),
                    "daily1am" => (s(0), s(1), "0 0 1 ? * *".into(), String::new()),
                    "daily2am" => (s(0), s(1), "0 0 2 ? * *".into(), String::new()),
                    "daily7am" => (s(0), s(1), "0 0 7 ? * *".into(), String::new()),
                    "dailyRandomBetween3and5" => (
                        s(0),
                        s(1),
                        "<random sec> <random min> <random 3-5> ? * *".into(),
                        String::new(),
                    ),
                    other => bail!("{}: unknown Defaults factory {other}", c.name),
                };
                row.insert("default_uid", uid);
                row.insert("default_name", dname);
                row.insert("default_cron", cron);
                row.insert("default_delay_seconds", delay);
            }
            other => bail!("{}: unexpected JobType args {other:?}", c.name),
        }
        row.insert("parameters_class", params);
        let flag = |m: &str| {
            flag_sets
                .get(m)
                .is_some_and(|v| v.contains(&c.name))
                .to_string()
        };
        row.insert("valid_user_required", flag("isValidUserRequiredForJob"));
        row.insert("using_notifications", flag("isUsingNotifications"));
        row.insert("using_error_notification", flag("isUsingErrorNotification"));
        row.insert(
            "using_continuous_execution",
            flag("isUsingContinuousExecution"),
        );
        row.insert(
            "relative_api_elements",
            relative.get(&c.name).cloned().unwrap_or_default(),
        );
        let mut im = impls.get(&c.name).cloned().unwrap_or_default();
        im.sort();
        im.dedup();
        row.insert("implementations", im.join(";"));
        rows.push(row);
    }
    Ok(rows)
}
