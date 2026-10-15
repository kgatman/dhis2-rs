//! `GET /api/system/info`: `SystemController#getSystemInfo` + `DefaultSystemService#getSystemInfo`.
//!
//! Values come from three places:
//! - per request, natively: system settings, server clock/zone, DB `now()`, request headers;
//! - at startup, natively: configuration-derived values and `DatabaseInfo`;
//! - **mirrored from Java** (ADR 0005): the values Java captures once at JVM start that describe
//!   the JVM or its build (`version`, `revision`, `buildTime`, `systemId`, and the superuser-only
//!   `jasperReportsVersion`, `java*`, `os*`, `memoryInfo`, `cpuCores`). They are fetched from
//!   Java's own `/api/system/info` with the requesting user's credentials, once.
//!
//! The rendered tree follows `SystemInfo`'s field order (Jackson serialisation order); nulls are
//! omitted; `fields=` is applied with the ported field filter over the class's dynamic schema.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::response::{IntoResponse, Response};
use dhis_auth::CurrentUser;
use dhis_config::ConfigurationKey as K;
use dhis_core_model::java::time::{
    format_iso_millis, now_millis, pretty_interval, system_tz_id, tz_display_name,
};
use dhis_core_model::settings::Settings;
use dhis_query::field_filter::{Node, filter, include_paths};
use http::{HeaderMap, HeaderValue, header};
use serde_json::Value;
use tokio::sync::OnceCell;

use crate::http::negotiation::{Representation, choose};
use crate::http::web_message::WebMessage;
use crate::http::{RequestContext, fallback};
use crate::params::Params;
use crate::{AppState, Authenticated};

const SYSTEM_INFO: &str = "org.hisp.dhis.system.SystemInfo";
const DATABASE_INFO: &str = "org.hisp.dhis.system.database.DatabaseInfo";
const CAPABILITY: &str = "org.hisp.dhis.system.capability.SystemCapability";

const PUBLIC_MIRRORED: &[&str] = &["version", "revision", "buildTime", "systemId"];
const SUPER_MIRRORED: &[&str] = &[
    "jasperReportsVersion",
    "javaVersion",
    "javaVendor",
    "javaOpts",
    "osName",
    "osArchitecture",
    "osVersion",
    "memoryInfo",
    "cpuCores",
];

/// Values computed once (Java: at startup) and kept for the life of the process.
#[derive(Default)]
pub struct SystemInfoCache {
    database: OnceCell<dhis_db::database_info::DatabaseInfo>,
    public_mirror: OnceCell<serde_json::Map<String, Value>>,
    super_mirror: OnceCell<serde_json::Map<String, Value>>,
}

/// `AnalyticsTableType` constants with `isLatestPartition()`, in enum order, and their setting
/// key suffixes (`SystemSettings#perTypeKeySuffix`).
const LATEST_PARTITION_TYPES: &[(&str, &str)] = &[
    ("DATA_VALUE", "DataValue"),
    ("COMPLETENESS", "Completeness"),
    ("EVENT", "Event"),
    (
        "TRACKED_ENTITY_INSTANCE_EVENTS",
        "TrackedEntityInstanceEvents",
    ),
];

/// `CalendarService` calendar names (`Calendar#name()` of the registered implementations).
const CALENDARS: &[&str] = &[
    "coptic",
    "ethiopian",
    "gregorian",
    "islamic",
    "iso8601",
    "julian",
    "nepali",
    "persian",
    "thai",
];

fn s(v: impl Into<String>) -> Node {
    Node::Scalar(Value::String(v.into()))
}

fn date(ms: i64) -> Node {
    s(format_iso_millis(ms))
}

/// Builder for a bean's properties that drops nulls (`JsonInclude.Include.NON_NULL`).
struct Props(Vec<(String, Node)>);

impl Props {
    fn put(&mut self, name: &str, v: Option<Node>) {
        if let Some(v) = v {
            self.0.push((name.to_string(), v));
        }
    }
}

fn mirrored(m: Option<&serde_json::Map<String, Value>>, name: &str) -> Option<Node> {
    m.and_then(|m| m.get(name))
        .filter(|v| !v.is_null())
        .cloned()
        .map(Node::Scalar)
}

async fn fetch_mirror(
    state: &AppState,
    headers: &HeaderMap,
    fields: &[&str],
) -> Option<serde_json::Map<String, Value>> {
    let java = state.java.as_ref()?;
    let doc = java
        .fetch_json(
            &format!("/api/system/info?fields={}", fields.join(",")),
            headers,
        )
        .await?;
    doc.as_object().cloned()
}

pub async fn get_system_info(
    State(state): State<Arc<AppState>>,
    Authenticated(user): Authenticated,
    params: Params,
    req: Request,
) -> Response {
    let ctx = req.extensions().get::<RequestContext>();
    // produces = {application/json, application/javascript}; JSONP is not ported.
    if choose(req.headers(), ctx, &[Representation::Json]) != Some(Representation::Json) {
        return fallback("content negotiation");
    }
    let Some(db) = state.db.as_ref() else {
        return fallback("no database");
    };
    // capability.analyticsTotalCounts = SqlBuilder#isHighPerformance: false for PostgreSQL,
    // true for ClickHouse/Doris. Only the PostgreSQL case is ported.
    if state.config.get_property(K::ANALYTICS_DATABASE).as_deref() != Some("POSTGRESQL") {
        return fallback("non-PostgreSQL analytics database");
    }
    // `@RequestParam(defaultValue = "*") String fields`: missing or empty → "*".
    let fields = params
        .first("fields")
        .filter(|f| !f.is_empty())
        .unwrap_or("*");

    let headers = req.headers().clone();
    let Some(public) = state
        .system_info
        .public_mirror
        .get_or_try_init(|| async {
            fetch_mirror(&state, &headers, PUBLIC_MIRRORED)
                .await
                .ok_or(())
        })
        .await
        .ok()
    else {
        return fallback("system info mirror unavailable");
    };
    let superuser = user.is_super();
    let super_values = if superuser {
        match state
            .system_info
            .super_mirror
            .get_or_try_init(|| async {
                fetch_mirror(&state, &headers, SUPER_MIRRORED)
                    .await
                    .ok_or(())
            })
            .await
        {
            Ok(m) => Some(m),
            Err(()) => return fallback("system info mirror unavailable"),
        }
    } else {
        None
    };

    let settings = match dhis_db::settings::system_settings(db).await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("loading system settings: {e}");
            return fallback("database error");
        }
    };
    let db_info = state
        .system_info
        .database
        .get_or_init(|| dhis_db::database_info::load(db, &state.config))
        .await
        .clone();
    let db_now = match dhis_db::database_info::now_millis(db).await {
        Ok(n) => n,
        Err(e) => {
            tracing::error!("select now(): {e}");
            return fallback("database error");
        }
    };

    let node = build(
        &state,
        &settings,
        &db_info,
        db_now,
        &headers,
        public,
        super_values,
        &user,
    );

    let mut resp = match render(&node, fields) {
        Ok(json) => crate::http::json_response(json),
        Err(RenderError::Parse(msg)) => WebMessage::bad_request(msg).into_response(),
        Err(RenderError::Unsupported) => return fallback("field transformers"),
    };
    // setNoStore(response) runs before the fields are parsed, so errors carry it too.
    resp.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-cache, no-store, max-age=0, must-revalidate"),
    );
    resp
}

enum RenderError {
    Parse(String),
    Unsupported,
}

fn render(node: &Node, fields: &str) -> Result<String, RenderError> {
    let paths = dhis_query::fields::parse(fields).map_err(|e| RenderError::Parse(e.0))?;
    let includes = include_paths(&paths, SYSTEM_INFO).map_err(|_| RenderError::Unsupported)?;
    Ok(filter(node, &includes).to_json_string())
}

#[allow(clippy::too_many_arguments)]
fn build(
    state: &AppState,
    settings: &Settings,
    db_info: &dhis_db::database_info::DatabaseInfo,
    db_now: i64,
    headers: &HeaderMap,
    public: &serde_json::Map<String, Value>,
    super_values: Option<&serde_json::Map<String, Value>>,
    user: &CurrentUser,
) -> Node {
    let config = &state.config;
    let superuser = user.is_super();
    let now = now_millis();
    let tz_id = system_tz_id();

    let calendar = settings.system_string("keyCalendar");
    let calendar = if CALENDARS.contains(&calendar.as_str()) {
        calendar
    } else {
        "iso8601".to_string()
    };
    let date_format = match settings.system_string("keyDateFormat").as_str() {
        "dd-MM-yyyy" => "dd-mm-yyyy",
        _ => "yyyy-mm-dd",
    };
    let last_tables = settings.system_date("keyLastSuccessfulAnalyticsTablesUpdate");
    let last_partition = settings.system_date("keyLastSuccessfulLatestAnalyticsPartitionUpdate");
    let by_type = |base: &str| {
        Node::Map(
            LATEST_PARTITION_TYPES
                .iter()
                .map(|(t, suffix)| {
                    (
                        t.to_string(),
                        date(settings.system_date(&format!("{base}{suffix}"))),
                    )
                })
                .collect(),
        )
    };
    let sync_ok = settings.system_date("keyLastMetaDataSyncSuccess");
    let sync_failed = settings.system_date("keyMetadataLastFailedTime");

    let mut db = Props(Vec::new());
    if superuser {
        db.put("name", db_info.name.clone().map(s));
        db.put("user", db_info.user.clone().map(s));
        db.put("url", db_info.url.clone().map(s));
        db.put("databaseVersion", db_info.database_version.clone().map(s));
    }
    db.put(
        "spatialSupport",
        Some(Node::Scalar(Value::Bool(db_info.spatial_support))),
    );
    db.put("time", Some(date(db_now)));

    let redis_enabled = config.is_enabled(K::REDIS_ENABLED);
    let sensitive = |v: Option<Node>| if superuser { v } else { None };
    let cfg = |k: K| config.get_property(k).map(s);

    let mut p = Props(Vec::new());
    p.put(
        "contextPath",
        Some(s(crate::http::paths::context_path(headers))),
    );
    p.put(
        "userAgent",
        headers
            .get(header::USER_AGENT)
            .and_then(|v| v.to_str().ok())
            .map(s),
    );
    p.put("calendar", Some(s(calendar)));
    p.put("dateFormat", Some(s(date_format)));
    p.put("serverDate", Some(date(now)));
    p.put("serverTimeZoneId", Some(s(tz_id.clone())));
    p.put(
        "sessionTimeout",
        config
            .get_int_property(K::SYSTEM_SESSION_TIMEOUT)
            .ok()
            .map(|v| Node::Scalar(v.into())),
    );
    p.put("serverTimeZoneDisplayName", tz_display_name(&tz_id).map(s));
    p.put("lastAnalyticsTableSuccess", Some(date(last_tables)));
    p.put(
        "intervalSinceLastAnalyticsTableSuccess",
        Some(s(pretty_interval(last_tables, now))),
    );
    p.put(
        "lastAnalyticsTableRuntime",
        Some(s(
            settings.system_string("keyLastSuccessfulAnalyticsTablesRuntime")
        )),
    );
    p.put(
        "lastSystemMonitoringSuccess",
        Some(date(
            settings.system_date("keyLastSuccessfulSystemMonitoringPush"),
        )),
    );
    p.put(
        "lastAnalyticsTablePartitionSuccess",
        Some(date(last_partition)),
    );
    p.put(
        "intervalSinceLastAnalyticsTablePartitionSuccess",
        Some(s(pretty_interval(last_partition, now))),
    );
    p.put(
        "lastAnalyticsTablePartitionRuntime",
        Some(s(settings.system_string(
            "keyLastSuccessfulLatestAnalyticsPartitionRuntime",
        ))),
    );
    p.put(
        "lastAnalyticsTableSuccessByType",
        Some(by_type("keyLastSuccessfulAnalyticsTablesUpdate")),
    );
    p.put(
        "lastAnalyticsTablePartitionSuccessByType",
        Some(by_type("keyLastSuccessfulLatestAnalyticsPartitionUpdate")),
    );
    p.put(
        "databaseInfo",
        Some(Node::Bean {
            klass: DATABASE_INFO,
            props: db.0,
        }),
    );
    // SqlBuilder#isHighPerformance of PostgreSqlBuilder (other databases fall back, see above).
    p.put(
        "capability",
        Some(Node::Bean {
            klass: CAPABILITY,
            props: vec![(
                "analyticsTotalCounts".into(),
                Node::Scalar(Value::Bool(false)),
            )],
        }),
    );
    p.put("version", mirrored(Some(public), "version"));
    p.put("revision", mirrored(Some(public), "revision"));
    p.put("buildTime", mirrored(Some(public), "buildTime"));
    p.put(
        "jasperReportsVersion",
        mirrored(super_values, "jasperReportsVersion"),
    );
    p.put(
        "environmentVariable",
        sensitive(Some(s(dhis_config::location::DEFAULT_ENV_VAR))),
    );
    p.put("fileStoreProvider", sensitive(cfg(K::FILESTORE_PROVIDER)));
    p.put("readOnlyMode", sensitive(cfg(K::SYSTEM_READ_ONLY_MODE)));
    p.put("nodeId", sensitive(cfg(K::NODE_ID)));
    for name in [
        "javaVersion",
        "javaVendor",
        "javaOpts",
        "osName",
        "osArchitecture",
        "osVersion",
    ] {
        p.put(name, mirrored(super_values, name));
    }
    p.put(
        "externalDirectory",
        sensitive(Some(s(config
            .home()
            .and_then(|h| std::path::absolute(h).ok())
            .map(|h| h.display().to_string())
            .unwrap_or_else(|| "Not set".into())))),
    );
    p.put(
        "readReplicaCount",
        sensitive(
            config
                .get_property(K::ACTIVE_READ_REPLICAS)
                .and_then(|v| dhis_core_model::java::number::integer_value_of(&v).ok())
                .map(|v| Node::Scalar(v.into())),
        ),
    );
    p.put("memoryInfo", mirrored(super_values, "memoryInfo"));
    p.put("cpuCores", mirrored(super_values, "cpuCores"));
    p.put(
        "emailConfigured",
        Some(Node::Scalar(Value::Bool(
            !settings.system_string("keyEmailHostName").trim().is_empty()
                && !settings.system_string("keyEmailUsername").trim().is_empty(),
        ))),
    );
    p.put(
        "redisEnabled",
        Some(Node::Scalar(Value::Bool(superuser && redis_enabled))),
    );
    p.put(
        "redisHostname",
        sensitive(if redis_enabled {
            cfg(K::REDIS_HOST)
        } else {
            None
        }),
    );
    p.put("systemId", mirrored(Some(public), "systemId"));
    p.put(
        "systemName",
        Some(s(settings.system_string("applicationTitle"))),
    );
    p.put(
        "systemMetadataVersion",
        Some(s(settings.system_string("keySystemMetadataVersion"))),
    );
    p.put("instanceBaseUrl", config.server_base_url().map(s));
    p.put(
        "systemMonitoringUrl",
        sensitive(cfg(K::SYSTEM_MONITORING_URL)),
    );
    p.put("clusterHostname", sensitive(cfg(K::CLUSTER_HOSTNAME)));
    p.put(
        "isMetadataVersionEnabled",
        Some(Node::Scalar(Value::Bool(
            settings.system_bool("keyVersionEnabled"),
        ))),
    );
    p.put(
        "lastMetadataVersionSyncAttempt",
        Some(date(if sync_ok < sync_failed {
            sync_failed
        } else {
            sync_ok
        })),
    );
    // isMetadataSyncEnabled is never set by DefaultSystemService (always null).
    p.put(
        "encryption",
        Some(Node::Scalar(Value::Bool(
            superuser && config.encryption_status() == dhis_config::EncryptionStatus::Ok,
        ))),
    );
    Node::Bean {
        klass: SYSTEM_INFO,
        props: p.0,
    }
}
