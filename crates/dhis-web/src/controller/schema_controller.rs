//! Ported from `org.hisp.dhis.webapi.controller.SchemaController` (partial).
//!
//! | Java handler | Mapping | Status |
//! |---|---|---|
//! | `getSchemas` | `GET /api/schemas` | JSON ported; XML falls back |
//! | `getSchema` | `GET /api/schemas/{type}` | JSON ported; XML falls back |
//! | `getSchemaProperty` | `GET /api/schemas/{type}/{property}` | JSON ported; XML falls back |
//! | `validateSchema` | `POST/PUT /api/schemas/{type}` | Java |
//!
//! The schemas are the ones Java registers at startup, captured from the pinned build (ADR 0007),
//! rendered through the field filter over the dynamic schemas of `Schema`/`Property`. Links are
//! generated per request (`DefaultLinkService#generateSchemaLinks`) from the request's API path.
//!
//! Java writes the links into its shared `Schema`/`Property` objects, and `getSchemaProperty`
//! does not generate any: it shows the links of the last `getSchemas`/`getSchema` request that
//! covered that schema (any client, any host), or none since startup. That state is kept here
//! ([`LINKS`]); it must move with the other two handlers, or Java's copy would stop being updated.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, RwLock};

use axum::Router;
use axum::extract::rejection::PathRejection;
use axum::extract::{Path, Request};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use dhis_query::field_filter::{Node, filter, include_paths};
use dhis_schema::CAPTURE_API_PATH;
use serde_json::Value;

use crate::http::negotiation::{Representation, choose};
use crate::http::paths::api_path;
use crate::http::web_message::WebMessage;
use crate::http::{RequestContext, fallback, json_response};
use crate::params::Params;
use crate::{AppState, Authenticated};

const SCHEMA: &str = "org.hisp.dhis.schema.Schema";

/// The registered schemas in `getSortedSchemas` order.
static SCHEMAS: LazyLock<Vec<Node>> =
    LazyLock::new(|| {
        match dhis_query::json_tree::bean_from_json(dhis_schema::REGISTERED_SCHEMAS_JSON, SCHEMA) {
            Ok(Node::Array(items)) => items,
            other => panic!("bad registered schemas data: {other:?}"),
        }
    });

/// `schemaBySingularName` (exact, case-sensitive).
static BY_SINGULAR: LazyLock<HashMap<String, usize>> = LazyLock::new(|| {
    SCHEMAS
        .iter()
        .enumerate()
        .filter_map(|(i, n)| match n {
            Node::Bean { props, .. } => props.iter().find_map(|(k, v)| match (k.as_str(), v) {
                ("singular", Node::Scalar(Value::String(s))) => Some((s.clone(), i)),
                _ => None,
            }),
            _ => None,
        })
        .collect()
});

/// Per schema (index into [`SCHEMAS`]): the API path of the last link generation, `None` before
/// the first (`DefaultLinkService#generateSchemaLinks` mutates the shared objects).
static LINKS: LazyLock<RwLock<Vec<Option<String>>>> =
    LazyLock::new(|| RwLock::new(vec![None; SCHEMAS.len()]));

fn set_links(indices: impl Iterator<Item = usize>, base: &str) {
    let mut links = LINKS.write().unwrap_or_else(|e| e.into_inner());
    for i in indices {
        links[i] = Some(base.to_string());
    }
}

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/schemas", get(get_schemas))
        .route("/api/schemas/{type}", get(get_schema))
        .route("/api/schemas/{type}/{property}", get(get_schema_property))
}

fn request_api_path(req: &Request) -> String {
    let request_uri = req
        .extensions()
        .get::<RequestContext>()
        .map_or_else(|| req.uri().path().to_string(), |c| c.original_path.clone());
    api_path(req.headers(), &request_uri)
}

fn json_acceptable(req: &Request) -> bool {
    choose(
        req.headers(),
        req.extensions().get::<RequestContext>(),
        &[Representation::Json],
    ) == Some(Representation::Json)
}

enum Rendered {
    Ok(Vec<Node>),
    Error(Response),
}

/// Field filtering (`FieldFilterService#toObjectNodes`) and link generation for `schemas`.
fn render(req: &Request, params: &Params, schemas: &[&Node], base: &str) -> Rendered {
    if !json_acceptable(req) {
        return Rendered::Error(fallback("content negotiation"));
    }
    // `@RequestParam(defaultValue = "*") String fields`: missing or empty → "*".
    let fields = params
        .first("fields")
        .filter(|f| !f.is_empty())
        .unwrap_or("*");
    let paths = match dhis_query::fields::parse(fields) {
        Ok(p) => p,
        Err(e) => return Rendered::Error(WebMessage::bad_request(e.0).into_response()),
    };
    let Ok(includes) = include_paths(&paths, SCHEMA) else {
        return Rendered::Error(fallback("field transformers"));
    };
    Rendered::Ok(
        schemas
            .iter()
            .map(|s| with_links(filter(s, &includes), Some(base)))
            .collect(),
    )
}

/// Members only `generateSchemaLinks` sets (on schemas: `href`, `apiEndpoint`; on reference
/// properties: those and `relativeApiEndpoint`).
const LINK_KEYS: &[&str] = &["href", "apiEndpoint", "relativeApiEndpoint"];

/// The captured links with the API path of the last link generation; without one (`None`), the
/// generated members are absent (null, so Jackson omits them).
fn with_links(node: Node, base: Option<&str>) -> Node {
    match node {
        Node::Bean { klass, props } => Node::Bean {
            klass,
            props: props
                .into_iter()
                .filter(|(k, v)| {
                    base.is_some()
                        || !(LINK_KEYS.contains(&k.as_str())
                            && klass == "org.hisp.dhis.schema.Property"
                            && matches!(v, Node::Scalar(_)))
                })
                .map(|(k, v)| (k, with_links(v, base)))
                .collect(),
        },
        Node::Map(props) => Node::Map(
            props
                .into_iter()
                .map(|(k, v)| (k, with_links(v, base)))
                .collect(),
        ),
        Node::Array(items) => Node::Array(items.into_iter().map(|i| with_links(i, base)).collect()),
        Node::Scalar(Value::String(s)) => match (s.strip_prefix(CAPTURE_API_PATH), base) {
            (Some(rest), Some(base)) => Node::Scalar(Value::String(format!("{base}{rest}"))),
            _ => Node::Scalar(Value::String(s)),
        },
        other => other,
    }
}

/// `getSchemas`: `{"schemas": [...]}` (`JsonRoot.of("schemas", ...)`).
async fn get_schemas(_user: Authenticated, params: Params, req: Request) -> Response {
    // The handler has no `produces`, so it runs (and generates links) whatever the Accept; a
    // fallback replays the request to Java, which then does the same.
    let base = request_api_path(&req);
    set_links(0..SCHEMAS.len(), &base);
    let all: Vec<&Node> = SCHEMAS.iter().collect();
    match render(&req, &params, &all, &base) {
        Rendered::Ok(nodes) => {
            let mut body = String::from("{\"schemas\":");
            Node::Array(nodes).write_json(&mut body);
            body.push('}');
            json_response(body)
        }
        Rendered::Error(r) => r,
    }
}

/// `getSchema`: one schema by singular name, or 404 `Type <type> does not exist.`.
async fn get_schema(
    _user: Authenticated,
    ty: Result<Path<String>, PathRejection>,
    params: Params,
    req: Request,
) -> Response {
    let Ok(Path(ty)) = ty else {
        return fallback("path variable");
    };
    let Some(&i) = BY_SINGULAR.get(&ty) else {
        return type_not_found(&req, &ty);
    };
    let base = request_api_path(&req);
    set_links(std::iter::once(i), &base);
    match render(&req, &params, &[&SCHEMAS[i]], &base) {
        Rendered::Ok(mut nodes) => json_response(nodes.remove(0).to_json_string()),
        Rendered::Error(r) => r,
    }
}

/// `getSchemaFromType`'s 404; the WebMessage is JSON only when JSON is acceptable.
fn type_not_found(req: &Request, ty: &str) -> Response {
    if !json_acceptable(req) {
        return fallback("content negotiation");
    }
    WebMessage::not_found(format!("Type {ty} does not exist.")).into_response()
}

/// `getSchemaProperty`: the `Property` as Jackson writes it (no field filter), looked up by
/// `Property#key()` (`collectionName` for collections, else `name`), with the schema's current
/// links.
async fn get_schema_property(
    _user: Authenticated,
    path: Result<Path<(String, String)>, PathRejection>,
    req: Request,
) -> Response {
    let Ok(Path((ty, property))) = path else {
        return fallback("path variable");
    };
    let Some(&i) = BY_SINGULAR.get(&ty) else {
        return type_not_found(&req, &ty);
    };
    let found = match &SCHEMAS[i] {
        Node::Bean { props, .. } => {
            props
                .iter()
                .find(|(k, _)| k == "properties")
                .and_then(|(_, v)| match v {
                    Node::Array(items) => items.iter().find(|p| property_key(p) == Some(&property)),
                    _ => None,
                })
        }
        _ => None,
    };
    if !json_acceptable(&req) {
        return fallback("content negotiation");
    }
    let Some(found) = found else {
        return WebMessage::not_found(format!("Property {property} does not exist on type {ty}."))
            .into_response();
    };
    let base = LINKS.read().unwrap_or_else(|e| e.into_inner())[i].clone();
    json_response(with_links(found.clone(), base.as_deref()).to_json_string())
}

/// `Property#key()`.
fn property_key(node: &Node) -> Option<&String> {
    let Node::Bean { props, .. } = node else {
        return None;
    };
    let get = |name: &str| props.iter().find(|(k, _)| k == name).map(|(_, v)| v);
    let key = if matches!(get("collection"), Some(Node::Scalar(Value::Bool(true)))) {
        "collectionName"
    } else {
        "name"
    };
    match get(key) {
        Some(Node::Scalar(Value::String(s))) => Some(s),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registered_schemas_load() {
        assert_eq!(SCHEMAS.len(), 116);
        assert!(BY_SINGULAR.contains_key("dataElement"));
        assert!(!BY_SINGULAR.contains_key("DataElement"));
    }
}
