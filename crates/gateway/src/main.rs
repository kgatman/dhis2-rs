use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use gateway::{Config, Gateway, RouteTable, ShadowConfig};
use tokio::net::TcpListener;
use tracing::{error, info, warn};

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
    /// diffcheck rules (TOML) used to compare Java and Rust on `shadow` routes.
    #[arg(long, env = "GATEWAY_SHADOW_RULES")]
    shadow_rules: Option<PathBuf>,
    /// Append shadow comparison results here (JSON lines).
    #[arg(long, env = "GATEWAY_SHADOW_LOG")]
    shadow_log: Option<PathBuf>,
    /// DHIS2_HOME holding dhis.conf, for the Rust handlers (stand-in for -Ddhis2.home; the
    /// DHIS2_HOME env var and /opt/dhis2 are used otherwise, as in Java).
    #[arg(long)]
    dhis2_home: Option<String>,
    /// Max connections of the Rust DB pool (Java's own pool uses connection.pool.max_size;
    /// both must fit in Postgres max_connections).
    #[arg(long, env = "GATEWAY_DB_POOL_MAX", default_value_t = 10)]
    db_pool_max: u32,
}

/// The Rust router, or one that hands everything back to Java when dhis.conf is not available
/// (e.g. a gateway deployed purely as a proxy).
fn rust_router(args: &Args) -> axum::Router {
    let opts = dhis_config::LoadOptions::from_process(args.dhis2_home.clone());
    let config = match dhis_config::DhisConfigurationProvider::load(&opts) {
        Ok(c) => c,
        Err(e) => {
            warn!("dhis.conf not loaded ({e}); every Rust route falls back to Java");
            return axum::Router::new()
                .fallback(|| async { dhis_web::http::fallback("rust side not configured") });
        }
    };
    let pool = match dhis_db::pool(&config, Some(args.db_pool_max)) {
        Ok(p) => p,
        Err(e) => {
            warn!("database pool not created ({e}); every Rust route falls back to Java");
            return axum::Router::new()
                .fallback(|| async { dhis_web::http::fallback("rust side not configured") });
        }
    };
    info!(home = ?config.home(), "Rust side configured");
    let java = Arc::new(dhis_auth::delegated::JavaAuthenticator::new(
        args.java_upstream.clone(),
    ));
    dhis_web::router(Arc::new(dhis_web::AppState {
        config: Arc::new(config),
        cors_whitelist: dhis_web::CorsWhitelistSource::Db(pool.clone()),
        db: Some(pool),
        java: Some(java),
        system_info: Default::default(),
    }))
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
            shadow: ShadowConfig {
                rules: match &args.shadow_rules {
                    Some(p) => diffcheck::Rules::load(p)?,
                    None => diffcheck::Rules::default(),
                },
                log_path: args.shadow_log.clone(),
                ..ShadowConfig::default()
            },
            delegate_auth: true,
        },
        routes,
        rust_router(&args),
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
        _ = shutdown_signal() => info!("shutting down"),
    }
    Ok(())
}

/// Ctrl-C, or SIGTERM (what `docker stop` and Kubernetes send).
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut term =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(s) => s,
                Err(e) => {
                    error!("cannot listen for SIGTERM: {e}");
                    let _ = tokio::signal::ctrl_c().await;
                    return;
                }
            };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
