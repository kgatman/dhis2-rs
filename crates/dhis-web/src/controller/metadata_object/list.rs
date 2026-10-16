//! `AbstractFullReadOnlyController#getObjectListInternal` for the requests the `GistBridge`
//! leaves to the metadata query engine (`QueryService`): the rows are planned by
//! [`dhis_query::metadata`], rendered like `getObject`'s and written as `StreamingJsonRoot`
//! (`{"pager":{...},"<collection>":[...]}`).

use std::collections::HashMap;

use axum::response::Response;
use dhis_auth::CurrentUser;
use dhis_query::gist::plan::SqlParam;
use dhis_query::metadata::{self, ListQuery};
use dhis_schema::hibernate;
use dhis_schema::registered::RegisteredSchema;
use http::{HeaderValue, header};

use super::{NotPorted, RequestParts, prepare};
use crate::AppState;
use crate::http::json_response;
use crate::http::paths::api_path;

/// The bound `GetObjectListParams`.
pub(crate) struct ListRequest {
    /// `getFieldsJsonList()`.
    pub fields: String,
    /// The `filter` values joined with commas (`setFilter(String)`).
    pub filter: String,
    /// The `order` values.
    pub orders: Vec<String>,
    /// `query`, unless empty.
    pub query: Option<String>,
    pub any: bool,
    pub paging: bool,
    pub page: i32,
    pub page_size: i32,
}

/// `Pager.DEFAULT_PAGE_SIZE`.
const DEFAULT_PAGE_SIZE: i32 = 50;

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

pub(crate) async fn object_list(
    state: &AppState,
    user: &CurrentUser,
    req: &RequestParts,
    schema: &'static RegisteredSchema,
    list: &ListRequest,
) -> Result<Response, NotPorted> {
    let db = state.db.as_ref().ok_or(NotPorted("no database"))?;
    let prep = prepare(user, schema, &list.fields)?;
    let entity = hibernate::entity(&schema.klass).ok_or(NotPorted("not mapped"))?;
    // UserSettings#getUserDbLocale, as Locale#toString
    let db_locale = prep.locale.to_string();
    let filters = metadata::split_filters(&list.filter);
    // getSharingPredicates: the groups come from getCurrentUserGroupInfo (the database, through
    // a cache invalidated on membership changes), not from the session
    let groups = if prep.acl_user.superuser {
        Vec::new()
    } else {
        dhis_db::users::group_uids_by_uid(db, prep.acl_user.uid)
            .await
            .map_err(|_| NotPorted("user groups"))?
    };
    let sharing_user = (!prep.acl_user.superuser).then_some((prep.acl_user.uid, &groups[..]));
    let plan = metadata::plan(&ListQuery {
        schema,
        entity,
        filters: &filters,
        query: list.query.as_deref(),
        orders: &list.orders,
        any: list.any,
        db_locale: &db_locale,
        sharing_user,
    })
    .map_err(|e| NotPorted(e.0))?;

    // GetObjectListParams#getPagination
    let paging = if list.paging {
        if list.page_size < 1 {
            return Err(NotPorted("page size below 1"));
        }
        let first = i64::from(list.page.max(1) - 1) * i64::from(list.page_size);
        let first = i32::try_from(first).map_err(|_| NotPorted("int overflow of the offset"))?;
        Some((first, list.page_size))
    } else {
        None
    };
    let mut sql = format!(
        "select {} from {} e{} where {} order by {}",
        prep.select_list(),
        prep.table,
        prep.sel.joins.concat(),
        plan.where_sql,
        plan.order_sql
    );
    if let Some((first, size)) = paging {
        sql.push_str(&format!(" limit {size} offset {first}"));
    }
    // SQL from mapping identifiers and validated literals; values are bound
    let rows = bind(sqlx::query(sqlx::AssertSqlSafe(sql)), &plan.params)
        .fetch_all(db)
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, "metadata list query failed");
            NotPorted("query failed")
        })?;
    let total = if paging.is_some() {
        let sql = format!(
            "select count(*) from {} e where {}",
            prep.table, plan.where_sql
        );
        let row = bind(sqlx::query(sqlx::AssertSqlSafe(sql)), &plan.params)
            .fetch_one(db)
            .await
            .map_err(|e| {
                tracing::warn!(error = %e, "metadata count query failed");
                NotPorted("count failed")
            })?;
        Some(sqlx::Row::try_get::<i64, _>(&row, 0).map_err(|_| NotPorted("count"))?)
    } else {
        None
    };

    let api = api_path(&req.headers, &req.original_path);
    let mut names = HashMap::new();
    let mut items = Vec::with_capacity(rows.len());
    for row in &rows {
        let cells = prep.cells(row)?;
        prep.sharing_display_names(db, &cells, &mut names).await?;
        items.push(prep.row_json(schema, &cells, &api, &names)?);
    }

    let mut out = String::from("{");
    if let Some(total) = total {
        let endpoint = format!(
            "{api}{}{}",
            schema.relative_api_endpoint.as_deref().unwrap_or(""),
            content_type_suffix(&req.original_path)
        );
        let parameters =
            parameters_string(req.query.as_deref()).ok_or(NotPorted("request parameters"))?;
        out.push_str("\"pager\":");
        out.push_str(&pager_json(
            list.page,
            total,
            list.page_size,
            &endpoint,
            &parameters,
        ));
        out.push(',');
    }
    let collection = schema
        .collection_name
        .as_deref()
        .unwrap_or(schema.plural.as_str());
    out.push_str(&serde_json::to_string(collection).map_err(|_| NotPorted("name"))?);
    out.push_str(":[");
    out.push_str(&items.join(","));
    out.push_str("]}");
    let mut r = json_response(out);
    r.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-cache, private"),
    );
    Ok(r)
}

/// `DefaultLinkService#getContentTypeSuffix`: from the first `.` of the request URI's last
/// segment.
fn content_type_suffix(request_uri: &str) -> &str {
    let last = request_uri
        .rfind('/')
        .map_or(request_uri, |i| &request_uri[i..]);
    last.find('.').map_or("", |i| &last[i..])
}

/// `new Pager(page, total, pageSize)` with `generatePagerLinks`, as the static JSON mapper writes
/// it (`page`, `total`, `pageSize`, `nextPage`, `prevPage`, `pageCount`; nulls left out).
fn pager_json(page: i32, total: i64, page_size: i32, endpoint: &str, parameters: &str) -> String {
    let total = total.max(0);
    let page_size = page_size.max(1);
    let page_count = (total as f64 / f64::from(page_size)).ceil() as i32;
    let page = page.min(page_count).max(1);
    let size = if page_size == DEFAULT_PAGE_SIZE {
        String::new()
    } else {
        format!("&pageSize={page_size}")
    };
    let amp = |p: &str| {
        if p.is_empty() {
            String::new()
        } else {
            format!("&{p}")
        }
    };
    let next = (page < page_count)
        .then(|| format!("{endpoint}?page={}{size}{}", page + 1, amp(parameters)));
    let prev = (page > 1).then(|| {
        if page - 1 == 1 {
            if page_size == DEFAULT_PAGE_SIZE {
                if parameters.is_empty() {
                    endpoint.to_string()
                } else {
                    format!("{endpoint}?{parameters}")
                }
            } else {
                format!("{endpoint}?pageSize={page_size}{}", amp(parameters))
            }
        } else {
            format!("{endpoint}?page={}{size}{}", page - 1, amp(parameters))
        }
    });
    let mut s = format!("{{\"page\":{page},\"total\":{total},\"pageSize\":{page_size}");
    let quote = |v: &str| serde_json::to_string(v).unwrap_or_default();
    if let Some(n) = next {
        s.push_str(&format!(",\"nextPage\":{}", quote(&n)));
    }
    if let Some(p) = prev {
        s.push_str(&format!(",\"prevPage\":{}", quote(&p)));
    }
    s.push_str(&format!(",\"pageCount\":{page_count}}}"));
    s
}

/// Tomcat's form decoding of one query component (`+` is a space); `None` for a malformed escape
/// or bytes that are no UTF-8 (Tomcat's handling of those is not reproduced).
fn tomcat_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' => {
                let hex = |c: u8| (c as char).to_digit(16);
                let hi = hex(*b.get(i + 1)?)?;
                let lo = hex(*b.get(i + 2)?)?;
                out.push((hi * 16 + lo) as u8);
                i += 2;
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8(out).ok()
}

/// `URLEncoder.encode(s, UTF-8)`.
fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &c in s.as_bytes() {
        match c {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'.' | b'-' | b'*' | b'_' => {
                out.push(c as char);
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{c:02X}")),
        }
    }
    out
}

/// `DefaultLinkService#getParametersString`: every parameter but `page` and `pageSize`, names in
/// the order of the `HashMap` that `getParameterValuesMap` collects them into, values in request
/// order, form-encoded. `None` where Tomcat's parsing is not reproduced.
fn parameters_string(query: Option<&str>) -> Option<String> {
    let mut params: Vec<(String, Vec<String>)> = Vec::new();
    for part in query.unwrap_or("").split('&') {
        if part.is_empty() {
            continue;
        }
        let (name, value) = part.split_once('=').unwrap_or((part, ""));
        let name = tomcat_decode(name)?;
        let value = tomcat_decode(value)?;
        if name.is_empty() {
            return None;
        }
        match params.iter_mut().find(|(n, _)| *n == name) {
            Some((_, vs)) => vs.push(value),
            None => params.push((name, vec![value])),
        }
    }
    let order = dhis_core_model::java::hash::hash_set_order(
        params.iter().map(|(n, _)| n.as_str()),
        dhis_core_model::java::hash::Construction::Default,
    )?;
    let mut out = String::new();
    for name in order {
        if name == "page" || name == "pageSize" {
            continue;
        }
        let (_, values) = params.iter().find(|(n, _)| n == name)?;
        for v in values {
            if !out.is_empty() {
                out.push('&');
            }
            out.push_str(&url_encode(name));
            out.push('=');
            out.push_str(&url_encode(v));
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pager_like_java() {
        // measured: /api/constants?fields=id,name&pageSize=2&page=2&query=Corpus (60 matches)
        let e = "http://localhost:8081/api/constants";
        let p = parameters_string(Some("fields=id,name&pageSize=2&page=2&query=Corpus")).unwrap();
        assert_eq!(p, "query=Corpus&fields=id%2Cname");
        assert_eq!(
            pager_json(2, 60, 2, e, &p),
            "{\"page\":2,\"total\":60,\"pageSize\":2,\
             \"nextPage\":\"http://localhost:8081/api/constants?page=3&pageSize=2&query=Corpus&fields=id%2Cname\",\
             \"prevPage\":\"http://localhost:8081/api/constants?pageSize=2&query=Corpus&fields=id%2Cname\",\
             \"pageCount\":30}"
        );
        assert_eq!(
            pager_json(99, 60, 2, e, &p),
            "{\"page\":30,\"total\":60,\"pageSize\":2,\
             \"prevPage\":\"http://localhost:8081/api/constants?page=29&pageSize=2&query=Corpus&fields=id%2Cname\",\
             \"pageCount\":30}"
        );
        assert_eq!(
            pager_json(1, 0, 2, e, ""),
            "{\"page\":1,\"total\":0,\"pageSize\":2,\"pageCount\":0}"
        );
    }

    #[test]
    fn parameters_like_tomcat_and_url_encoder() {
        assert_eq!(
            parameters_string(Some("filter=name:like:a+b&filter=x%2Ay&q")).unwrap(),
            "filter=name%3Alike%3Aa+b&filter=x*y&q="
        );
        assert!(parameters_string(Some("x=%zz")).is_none());
        assert_eq!(content_type_suffix("/api/constants.json"), ".json");
        assert_eq!(content_type_suffix("/api/constants"), "");
    }
}
