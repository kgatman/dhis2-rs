//! Ported from `AbstractFullReadOnlyController` / `AbstractGistReadOnlyController` (partial), for
//! the metadata types in [`GIST_TYPES`].
//!
//! | Java handler | Mapping | Status |
//! |---|---|---|
//! | `getObjectList` | `GET /api/{type}` | requests the `GistBridge` hands to Gist; others Java |
//! | `getObjectListGist` | `GET /api/{type}/gist` (JSON) | partly (see `dhis_query::gist::plan`) |
//! | `getObjectListCsv`, `getObject`, `getObjectProperty`, the other Gist handlers | | Java |
//!
//! Requests the Gist engine port cannot answer, including every request Java answers with an
//! error, go to Java (`dhis_query::gist::Unsupported`). Other users than superusers need the
//! session's identity for the sharing filter (ADR 0009); without it the request goes to Java.

use std::sync::Arc;

use axum::Router;
use axum::extract::{Request, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use dhis_auth::CurrentUser;
use dhis_query::gist::AutoType;
use dhis_query::gist::bridge::{self, BridgeInput, Decision};
use dhis_query::gist::output::{self, Cell, ListOutput, Pager, RequestUrl};
use dhis_query::gist::plan::{
    self, Access, ColumnType, JsonKind, ListParams, ListPlan, Slot, SqlParam,
};
use dhis_schema::registered::{self, RegisteredSchema};
use http::{HeaderValue, header};
use sqlx::Row;

use crate::http::negotiation::{Representation, choose};
use crate::http::paths::{context_path, versionless_request_uri};
use crate::http::{JavaFallback, RequestContext, fallback};
use crate::params::Params;
use crate::{AppState, Authenticated};

/// Metadata types (singular names) whose list endpoints are routed here.
pub const GIST_TYPES: &[&str] = &[
    "constant",
    "dataElement",
    "dataElementGroup",
    "option",
    "optionSet",
    "legendSet",
    "categoryOption",
    "category",
    "categoryCombo",
    "indicator",
    "indicatorType",
    "organisationUnit",
    "organisationUnitGroup",
    "userGroup",
    "userRole",
];

pub fn routes() -> Router<Arc<AppState>> {
    let mut r = Router::new();
    for singular in GIST_TYPES {
        let schema = registered::by_singular(singular).expect("registered schema");
        let endpoint = schema
            .relative_api_endpoint
            .as_deref()
            .expect("type with an endpoint");
        r = r
            .route(
                &format!("/api{endpoint}"),
                get(
                    move |state: State<Arc<AppState>>, user: Authenticated, req: Request| {
                        get_object_list(state, user, req, schema)
                    },
                ),
            )
            .route(
                &format!("/api{endpoint}/gist"),
                get(
                    move |state: State<Arc<AppState>>, user: Authenticated, req: Request| {
                        get_object_list_gist(state, user, req, schema)
                    },
                ),
            );
    }
    r
}

/// The paths whose handlers need the user settings next to the authorities (the translation
/// locale is the user's database locale).
pub fn needs_settings(normalized_path: &str) -> bool {
    GIST_TYPES.iter().any(|s| {
        registered::by_singular(s)
            .and_then(|schema| schema.relative_api_endpoint.as_deref())
            .is_some_and(|e| {
                normalized_path
                    .strip_prefix("/api")
                    .and_then(|p| p.strip_prefix(e))
                    .is_some_and(|rest| rest.is_empty() || rest == "/gist")
            })
    })
}

fn fb(reason: &'static str) -> JavaFallback {
    JavaFallback { reason }
}

/// Spring data binding of one bean property: strings join repeated values with commas, other
/// types are only taken from a single value in the plain form every converter agrees on.
struct Binder<'a> {
    params: &'a Params,
}

impl Binder<'_> {
    fn string(&self, name: &str) -> Option<String> {
        let all = self.params.all(name);
        (!all.is_empty()).then(|| all.join(","))
    }

    fn single(&self, name: &str) -> Result<Option<&str>, JavaFallback> {
        match self.params.all(name).as_slice() {
            [] => Ok(None),
            [v] => Ok(Some(v)),
            _ => Err(fb("repeated parameter")),
        }
    }

    fn int(&self, name: &str, default: i32) -> Result<i32, JavaFallback> {
        match self.single(name)? {
            None => Ok(default),
            Some(v) => {
                let digits = v.strip_prefix('-').unwrap_or(v);
                if digits.is_empty()
                    || digits.len() > 9
                    || !digits.bytes().all(|b| b.is_ascii_digit())
                {
                    return Err(fb("integer parameter"));
                }
                v.parse().map_err(|_| fb("integer parameter"))
            }
        }
    }

    fn boolean(&self, name: &str) -> Result<Option<bool>, JavaFallback> {
        match self.single(name)? {
            None => Ok(None),
            Some("true") => Ok(Some(true)),
            Some("false") => Ok(Some(false)),
            Some(_) => Err(fb("boolean parameter")),
        }
    }

    /// `rootJunction`: `Junction.Type` by exact name; `true` for `OR`.
    fn junction_or(&self) -> Result<bool, JavaFallback> {
        match self.single("rootJunction")? {
            None | Some("AND") => Ok(false),
            Some("OR") => Ok(true),
            Some(_) => Err(fb("rootJunction")),
        }
    }
}

/// `org.hisp.dhis.common.Locale.of(locale).toString()` for the `locale` parameter (`None` where
/// Java fails with 400).
fn locale_of(locale: &str) -> Option<String> {
    dhis_core_model::locale::Locale::parse(locale)
        .ok()
        .map(|l| l.to_string())
}

/// `GistPipeline.getTranslationLocale`: the `locale` parameter, else the session's database
/// locale (`keyDbLocale` of the user settings snapshot, ADR 0006).
pub(crate) fn translation_locale(user: &CurrentUser, locale_param: &str) -> Option<String> {
    if !locale_param.is_empty() {
        return locale_of(locale_param);
    }
    let raw = user.settings.as_ref()?.get("keyDbLocale")?;
    serde_json::from_str::<String>(raw).ok()
}

/// The parameters `GistPipeline#createListQuery` reads, bound from the request.
struct GistListRequest {
    params: ListParams,
    headless: bool,
    unwrap: bool,
    page_list_name: Option<String>,
}

fn bind_gist_params(
    binder: &Binder<'_>,
    user: &CurrentUser,
    context_root: String,
) -> Result<GistListRequest, JavaFallback> {
    if binder.boolean("orgUnitsTree")? == Some(true)
        || binder.boolean("orgUnitsOffline")? == Some(true)
    {
        return Err(fb("organisation unit tree/offline"));
    }
    let total = match (binder.boolean("totalPages")?, binder.boolean("total")?) {
        (Some(a), Some(b)) if a != b => return Err(fb("contradicting total parameters")),
        (Some(a), _) => a,
        (None, Some(b)) => b,
        (None, None) => false,
    };
    let auto = match binder.single("auto")? {
        None => AutoType::S,
        Some(a) => AutoType::parse(a).ok_or_else(|| fb("auto parameter"))?,
    };
    let locale = binder.string("locale").unwrap_or_default();
    let translation_locale =
        translation_locale(user, &locale).ok_or_else(|| fb("translation locale"))?;
    Ok(GistListRequest {
        params: ListParams {
            fields: binder.string("fields").unwrap_or_default(),
            filter: binder.string("filter").unwrap_or_default(),
            order: binder.string("order").unwrap_or_default(),
            page: binder.int("page", 1)?,
            page_size: binder.int("pageSize", 50)?,
            total,
            absolute_urls: binder.boolean("absoluteUrls")?.unwrap_or(false),
            references: binder.boolean("references")?.unwrap_or(true),
            any_filter: binder.junction_or()?,
            auto,
            translation_locale,
            context_root,
        },
        headless: binder.boolean("headless")?.unwrap_or(false),
        unwrap: binder.boolean("unwrap")?.unwrap_or(true),
        page_list_name: binder.string("pageListName"),
    })
}

pub(crate) fn json_requested(req: &Request) -> bool {
    choose(
        req.headers(),
        req.extensions().get::<RequestContext>(),
        &[Representation::Json],
    ) == Some(Representation::Json)
}

/// `DefaultGistAccessControl` inputs for `user` listing `schema`: `aclService.canRead(user,
/// type)` (any of the type's `READ` authorities, if it has some), and the session identity for
/// the sharing filter (`CurrentUser::identity`, ADR 0009).
fn access(user: &CurrentUser, schema: &RegisteredSchema) -> Result<Access, JavaFallback> {
    let authorities = user
        .authorities
        .as_ref()
        .ok_or_else(|| fb("no authorities"))?;
    let superuser = user.is_super() == Some(true);
    let read = schema.authorities_by_type("READ");
    Ok(Access {
        superuser,
        skip_sharing_check: user.is_authorized("Test_skipSharingCheck") == Some(true),
        can_read_type: superuser
            || read.is_empty()
            || read.iter().any(|a| authorities.iter().any(|u| u == a)),
        user: user
            .identity
            .as_ref()
            .map(|i| (i.uid.clone(), i.group_ids.clone())),
        authorities: authorities.clone(),
    })
}

/// `getObjectListGist`: `GET /api/{type}/gist`.
async fn get_object_list_gist(
    State(state): State<Arc<AppState>>,
    Authenticated(user): Authenticated,
    req: Request,
    schema: &'static RegisteredSchema,
) -> Response {
    if !json_requested(&req) {
        return fallback("content negotiation");
    }
    let access = match access(&user, schema) {
        Ok(a) => a,
        Err(r) => return r.into_response(),
    };
    let params = Params::parse(req.uri().query());
    let binder = Binder { params: &params };
    let request = match bind_gist_params(&binder, &user, context_path(req.headers())) {
        Ok(r) => r,
        Err(r) => return r.into_response(),
    };
    let Some(url) = request_url(&req) else {
        return fallback("request URL");
    };
    export_list(&state, url, schema, access, request).await
}

/// `getObjectList`: `GET /api/{type}`, for requests the `GistBridge` hands to Gist.
async fn get_object_list(
    State(state): State<Arc<AppState>>,
    Authenticated(user): Authenticated,
    req: Request,
    schema: &'static RegisteredSchema,
) -> Response {
    if !json_requested(&req) {
        return fallback("content negotiation");
    }
    let access = match access(&user, schema) {
        Ok(a) => a,
        Err(r) => return r.into_response(),
    };
    if !access.can_read_type {
        // ForbiddenException: "You don't have the proper permissions to read objects of this type."
        return fallback("type not readable");
    }
    let params = Params::parse(req.uri().query());
    let binder = Binder { params: &params };
    // GetObjectListParams binding (errors are Java's to report)
    let bound = (|| -> Result<_, JavaFallback> {
        let paging = binder.boolean("paging")?.unwrap_or(true);
        let gist = binder.boolean("gist")?;
        let page = binder.int("page", 1)?;
        let page_size = binder.int("pageSize", 50)?;
        let any_filter = binder.junction_or()?;
        match binder.single("defaults")? {
            None | Some("INCLUDE" | "EXCLUDE") => {}
            Some(_) => return Err(fb("defaults parameter")),
        }
        Ok((paging, gist, page, page_size, any_filter))
    })();
    let (paging, gist, page, page_size, any_filter) = match bound {
        Ok(b) => b,
        Err(r) => return r.into_response(),
    };
    // `query` adds a filter (getAdditionalFilters) and is no bridge parameter: never Gist
    // getFieldsJsonList(): the default preset is idName
    let fields = binder
        .string("fields")
        .filter(|f| !f.is_empty())
        .unwrap_or_else(|| "id,displayName".into());
    let filter = binder.string("filter").unwrap_or_default();
    let order = binder.string("order").unwrap_or_default();
    let names = params_names(&params);
    let decision = bridge::decide(&BridgeInput {
        schema,
        gist,
        paging,
        param_names: names,
        fields: &fields,
        filter: &filter,
        order: &order,
    });
    if decision != Decision::Gist {
        return fallback("metadata query engine");
    }
    // GistBridge.toObjectListParams
    let locale = binder.string("locale").unwrap_or_default();
    let Some(translation_locale) = translation_locale(&user, &locale) else {
        return fallback("translation locale");
    };
    let request = GistListRequest {
        params: ListParams {
            fields,
            filter,
            order,
            page,
            page_size,
            total: true,
            absolute_urls: false,
            references: false,
            any_filter,
            auto: AutoType::S,
            translation_locale,
            context_root: context_path(req.headers()),
        },
        headless: false,
        unwrap: false,
        page_list_name: None,
    };
    let Some(url) = request_url(&req) else {
        return fallback("request URL");
    };
    export_list(&state, url, schema, access, request).await
}

fn params_names(params: &Params) -> Vec<&str> {
    let mut seen: Vec<&str> = Vec::new();
    for n in params.names() {
        if !seen.contains(&n) {
            seen.push(n);
        }
    }
    seen
}

/// `GistPipeline#exportAsJson(GistObjectList.Input)`.
async fn export_list(
    state: &AppState,
    url: OwnedRequestUrl,
    schema: &'static RegisteredSchema,
    access: Access,
    request: GistListRequest,
) -> Response {
    let Some(db) = state.db.as_ref() else {
        return fallback("no database");
    };
    let plan = match plan::plan_list(schema, &request.params, access) {
        Ok(p) => p,
        Err(e) => return fallback(e.0),
    };
    // Java runs both in one read-only transaction; separate statements see the same committed
    // data unless a write lands in between (as with Java's READ COMMITTED default)
    let (rows, total) = tokio::join!(fetch(db, &plan), async {
        match &plan.count_sql {
            None => Ok(None),
            Some(sql) => count(db, sql, &plan.params).await.map(Some),
        }
    });
    let rows = match rows {
        Ok(r) => r,
        Err(reason) => return fallback(reason),
    };
    let total = match total {
        Ok(t) => t,
        Err(reason) => return fallback(reason),
    };
    // DefaultGistService#pager
    let mut pager = Pager {
        page: plan.page,
        page_size: plan.page_size,
        total,
        prev_page: None,
        next_page: None,
    };
    if schema.has_api_endpoint() {
        let link = |page| output::page_link(&url.as_ref(), page);
        if pager.page > 1 {
            match link(pager.page - 1) {
                Ok(l) => pager.prev_page = Some(l),
                Err(e) => return fallback(e.0),
            }
        }
        let has_rows = !rows.is_empty();
        if has_rows && pager.page_count().is_none_or(|c| c > pager.page) {
            match link(pager.page + 1) {
                Ok(l) => pager.next_page = Some(l),
                Err(e) => return fallback(e.0),
            }
        }
    }
    let collection_name = request
        .page_list_name
        .as_deref()
        .or(schema.collection_name.as_deref())
        .unwrap_or(&schema.plural);
    let body = match output::render_list(
        &plan,
        rows,
        &pager,
        &ListOutput {
            collection_name,
            headless: request.headless,
            unwrap: request.unwrap,
        },
    ) {
        Ok(b) => b,
        Err(e) => return fallback(e.0),
    };
    let mut r = Response::new(body.into());
    // lazyOutputStream: written to the servlet stream (no message converter), no-store
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json;charset=UTF-8"),
    );
    r.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-cache, no-store, max-age=0, must-revalidate"),
    );
    r
}

/// `ContextUtils#getRequestURL` parts: Tomcat's scheme (`http`, the gateway's connection) and
/// server name/port from `Host`, the request URI as `ApiVersionFilter` presents it.
struct OwnedRequestUrl {
    host: String,
    port: Option<u16>,
    path: String,
    query: Option<String>,
}

impl OwnedRequestUrl {
    fn as_ref(&self) -> RequestUrl<'_> {
        RequestUrl {
            scheme: "http",
            host: &self.host,
            port: self.port,
            path: &self.path,
            query: self.query.as_deref(),
        }
    }
}

fn request_url(req: &Request) -> Option<OwnedRequestUrl> {
    let host = req.headers().get(header::HOST)?.to_str().ok()?;
    if host.starts_with('[') {
        return None;
    }
    let (name, port) = match host.rsplit_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().ok()?),
        None => (host, 80),
    };
    let raw_path = req
        .extensions()
        .get::<RequestContext>()
        .map_or_else(|| req.uri().path().to_string(), |c| c.original_path.clone());
    Some(OwnedRequestUrl {
        host: name.to_string(),
        port: (port != 80).then_some(port),
        path: versionless_request_uri(&raw_path),
        query: req.uri().query().map(str::to_string),
    })
}

fn bind<'q>(
    mut q: sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments>,
    params: &'q [SqlParam],
) -> sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments> {
    for p in params {
        q = match p {
            SqlParam::Text(s) => q.bind(s.as_str()),
            SqlParam::Double(d) => q.bind(*d),
            SqlParam::Int(i) => q.bind(*i),
            SqlParam::Long(l) => q.bind(*l),
            SqlParam::Bool(b) => q.bind(*b),
        };
    }
    q
}

async fn count(db: &sqlx::PgPool, sql: &str, params: &[SqlParam]) -> Result<i32, &'static str> {
    // SQL built by dhis_query::gist::plan from mapping identifiers and validated literals;
    // values are bound
    let row = bind(sqlx::query(sqlx::AssertSqlSafe(sql.to_string())), params)
        .fetch_one(db)
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, "gist count query failed");
            "gist count query failed"
        })?;
    let n: i64 = row.try_get(0).map_err(|_| "gist count type")?;
    // Long#intValue
    Ok(n as i32)
}

async fn fetch(db: &sqlx::PgPool, plan: &ListPlan) -> Result<Vec<Vec<Cell>>, &'static str> {
    let rows = bind(
        sqlx::query(sqlx::AssertSqlSafe(plan.fetch_sql.clone())),
        &plan.params,
    )
    .fetch_all(db)
    .await
    .map_err(|e| {
        tracing::warn!(error = %e, sql = %plan.fetch_sql, "gist query failed");
        "gist query failed"
    })?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let mut cells = Vec::with_capacity(plan.slots.len());
        let mut col = 0;
        for slot in &plan.slots {
            let Slot::Sql(ty) = slot else {
                cells.push(Cell::Null);
                continue;
            };
            let cell = decode(&row, col, *ty).ok_or("gist column type")?;
            col += 1;
            cells.push(cell);
        }
        out.push(cells);
    }
    Ok(out)
}

pub(crate) fn decode(row: &sqlx::postgres::PgRow, i: usize, ty: ColumnType) -> Option<Cell> {
    fn get<'r, T: sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres>>(
        row: &'r sqlx::postgres::PgRow,
        i: usize,
    ) -> Option<Option<T>> {
        row.try_get::<Option<T>, _>(i).ok()
    }
    Some(match ty {
        ColumnType::Text => get::<String>(row, i)?.map_or(Cell::Null, Cell::Text),
        ColumnType::Double => get::<f64>(row, i)?.map_or(Cell::Null, Cell::Double),
        ColumnType::Int => get::<i32>(row, i)?.map_or(Cell::Null, |v| Cell::Int(v.into())),
        ColumnType::Long => get::<i64>(row, i)?.map_or(Cell::Null, Cell::Int),
        ColumnType::Bool => get::<bool>(row, i)?.map_or(Cell::Null, Cell::Bool),
        ColumnType::TextArray => get::<Vec<String>>(row, i)?.map_or(Cell::Null, Cell::Array),
        ColumnType::Json(kind) => match get::<String>(row, i)? {
            None => Cell::Null,
            // a value Java cannot read (e.g. an invalid locale) fails the request there
            Some(text) => Cell::Raw(json_value(&text, kind)?),
        },
        ColumnType::Timestamp => match get::<String>(row, i)? {
            None => Cell::Null,
            Some(t) => Cell::Text(java_date(&t)?),
        },
    })
}

/// A JSONB column value as the Gist API writes the Java value it is read into.
fn json_value(text: &str, kind: JsonKind) -> Option<String> {
    use dhis_core_model::jsonb;
    match kind {
        JsonKind::Sharing => jsonb::sharing_json(text),
        JsonKind::Translations { wrapped } => jsonb::translations_json(text, wrapped),
        JsonKind::AttributeValues => {
            // LazyAttributeValues#addTo: an object of strings (json-tree escaping)
            let mut out = String::from("{");
            for (i, (k, v)) in jsonb::attribute_values(text)?.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                output::write_string(&mut out, k);
                out.push(':');
                output::write_string(&mut out, v);
            }
            out.push('}');
            Some(out)
        }
    }
}

/// A `timestamp` (as `YYYY-MM-DD HH24:MI:SS.US`) the way Java renders it: read into a
/// `java.util.Date` in the JVM zone (sub-millisecond digits dropped), printed with
/// `DateUtils.toIso8601NoTz` in the same zone.
fn java_date(text: &str) -> Option<String> {
    use dhis_core_model::java::time::{SYSTEM_TZ, format_iso_millis_in, parse_local_date_time};
    let (date, time) = text.split_once(' ')?;
    if date.len() != 10 {
        // years beyond 9999 or BC: not reproduced
        return None;
    }
    let (hms, frac) = time.split_once('.').unwrap_or((time, "000"));
    let millis = frac.get(..3)?;
    let millis = parse_local_date_time(&format!("{date}T{hms}.{millis}"), &SYSTEM_TZ)?;
    Some(format_iso_millis_in(millis, &SYSTEM_TZ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_drop_microseconds() {
        // the server zone in tests is the JVM default of this machine; GMT-like zones keep the
        // wall-clock time
        let d = java_date("2026-10-15 22:04:51.032478").unwrap();
        assert!(d.ends_with(".032") || d.len() == 23, "{d}");
        assert_eq!(java_date("12345-01-01 00:00:00.000000"), None);
    }

    #[test]
    fn locale_parameter() {
        // measured: locale=FR is a 400 ("Invalid language code: FR")
        assert_eq!(locale_of("FR"), None);
        assert_eq!(locale_of("fr").as_deref(), Some("fr"));
        assert_eq!(locale_of("es_419").as_deref(), Some("es_419"));
        assert_eq!(locale_of("iw").as_deref(), Some("he"));
        assert_eq!(locale_of("é"), None);
    }

    #[test]
    fn settings_paths() {
        assert!(needs_settings("/api/constants"));
        assert!(needs_settings("/api/constants/gist"));
        assert!(!needs_settings("/api/constants/abc"));
        assert!(!needs_settings("/api/constantsx"));
    }
}
