use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use gateway::{Config, Gateway, RouteTable};
use tokio::net::TcpListener;
use tracing::{error, info};

#[derive(Parser)]
#[command(about = "DHIS2 strangler-fig gateway: Java by default, Rust per route table")]
struct Args {
    /// Address to listen on.
    #[arg(long, env = "GATEWAY_LISTEN", default_value = "0.0.0.0:8080")]
    listen: String,
    /// Java server base URL (scheme and authority only).
    #[arg(long, env = "GATEWAY_JAVA_UPSTREAM")]
    java_upstream: http::Uri,
    /// Route table (TOML). Without it, every request goes to Java. Reloaded on SIGHUP.
    #[arg(long, env = "GATEWAY_ROUTES")]
    routes: Option<PathBuf>,
    /// Add X-Forwarded-For with the peer address when the client did not send one.
    #[arg(long, env = "GATEWAY_INJECT_FORWARDED_FOR", default_value_t = true, action = clap::ArgAction::Set)]
    inject_forwarded_for: bool,
}

fn load_routes(path: Option<&PathBuf>) -> Result<RouteTable> {
    match path {
        Some(p) => RouteTable::load(p).with_context(|| format!("loading {}", p.display())),
        None => Ok(RouteTable::all_java()),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let args = Args::parse();
    anyhow::ensure!(
        args.java_upstream.scheme_str() == Some("http") && args.java_upstream.authority().is_some(),
        "--java-upstream must be an http://host:port URL"
    );

    let routes = load_routes(args.routes.as_ref())?;
    info!(rules = routes.rules().len(), "route table loaded");
    let gw = Arc::new(Gateway::new(
        Config {
            java_upstream: args.java_upstream.clone(),
            inject_forwarded_for: args.inject_forwarded_for,
        },
        routes,
        dhis_web::router(),
    ));

    #[cfg(unix)]
    {
        let gw = gw.clone();
        let path = args.routes.clone();
        let mut hup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())?;
        tokio::spawn(async move {
            while hup.recv().await.is_some() {
                match load_routes(path.as_ref()) {
                    Ok(r) => {
                        info!(rules = r.rules().len(), "route table reloaded");
                        gw.set_routes(r);
                    }
                    Err(e) => error!("route table reload failed, keeping the old one: {e:#}"),
                }
            }
        });
    }

    let listener = TcpListener::bind(&args.listen)
        .await
        .with_context(|| format!("binding {}", args.listen))?;
    info!(listen = %args.listen, java = %args.java_upstream, "gateway started");
    tokio::select! {
        r = gw.serve(listener) => r.context("accept loop failed")?,
        _ = tokio::signal::ctrl_c() => info!("shutting down"),
    }
    Ok(())
}
