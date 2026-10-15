use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::Result;
use clap::Parser;
use diffcheck::{Outcome, Rules, Target, compare, corpus};

#[derive(Parser)]
#[command(about = "Replay a request corpus against Java and Rust and diff the responses")]
struct Args {
    /// Java base URL, e.g. http://localhost:8080
    #[arg(long, env = "DIFFCHECK_JAVA")]
    java: http::Uri,
    /// Rust base URL (the Rust server, or a gateway routing everything to Rust).
    #[arg(long, env = "DIFFCHECK_RUST")]
    rust: http::Uri,
    /// Corpus files (JSON lines).
    #[arg(required = true)]
    corpus: Vec<PathBuf>,
    /// Normalisation rules (TOML).
    #[arg(long)]
    rules: Option<PathBuf>,
    /// Only run cases with this tag (repeatable).
    #[arg(long)]
    tag: Vec<String>,
    /// Basic auth username applied to cases without an Authorization header.
    #[arg(long, env = "DIFFCHECK_USER", default_value = "admin")]
    user: String,
    #[arg(long, env = "DIFFCHECK_PASSWORD", default_value = "district")]
    password: String,
    /// Host header for every request, so both servers see the same Host (e.g. localhost:8081).
    #[arg(long)]
    host: Option<String>,
    /// Per-request timeout in seconds.
    #[arg(long, default_value_t = 120)]
    timeout: u64,
}

#[tokio::main]
async fn main() -> Result<ExitCode> {
    let args = Args::parse();
    let rules = match &args.rules {
        Some(p) => Rules::load(p)?,
        None => Rules::default(),
    };
    let timeout = Duration::from_secs(args.timeout);
    let auth = Some((args.user.as_str(), args.password.as_str()));
    let mut java = Target::new(args.java.clone(), auth, timeout)?;
    let mut rust = Target::new(args.rust.clone(), auth, timeout)?;
    if let Some(h) = &args.host {
        java.host = Some(h.parse()?);
        rust.host = Some(h.parse()?);
    }

    let mut cases = Vec::new();
    for p in &args.corpus {
        cases.extend(corpus::load(p)?);
    }
    if !args.tag.is_empty() {
        cases.retain(|c| c.tags.iter().any(|t| args.tag.contains(t)));
    }

    let (mut passed, mut failed) = (0usize, 0usize);
    // Sequential on purpose: write requests must not interleave.
    for case in &cases {
        let label = format!("{} {} ({})", case.method, case.path, case.name);
        let (j, r) = (java.send(case).await, rust.send(case).await);
        match (j, r) {
            (Ok(j), Ok(r)) => match compare(case, &j, &r, &rules) {
                Outcome::Pass => {
                    passed += 1;
                    println!("PASS {label}");
                }
                Outcome::Fail(diffs) => {
                    failed += 1;
                    println!("FAIL {label}");
                    for d in diffs {
                        println!("  {}", d.replace('\n', "\n  "));
                    }
                }
            },
            (j, r) => {
                failed += 1;
                println!("ERROR {label}");
                if let Err(e) = j {
                    println!("  java: {e:#}");
                }
                if let Err(e) = r {
                    println!("  rust: {e:#}");
                }
            }
        }
    }
    println!("\n{passed} passed, {failed} failed, {} total", cases.len());
    Ok(if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}
