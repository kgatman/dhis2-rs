//! `AbstractFullReadOnlyController#getObject`: `GET /api/{type}/{uid}`, for the types in
//! [`GIST_TYPES`](super::metadata_controller::GIST_TYPES).
//!
//! Java loads the entity (sharing-filtered: an unreadable object is a 404), fills `access` and the
//! sharing display names when they are requested, and serialises it with the field filter's copy of
//! its JSON mapper (`FieldFilterService`). Here the entity's row is read with SQL built from the
//! generated mappings, turned into a [`Node`] tree in the mapper's property order
//! (`dhis_schema::jackson`) and filtered with the ported `FieldPathHelper`.
//!
//! Supported so far: simple persisted properties, `sharing` (with display names), `translations`,
//! `attributeValues`, references to users (`UserPropertyTransformer`) and the ids of other
//! references, `displayName`/`displayShortName`/`displayDescription`/`displayFormName`, `user`,
//! `access`, `href`. Anything else that the requested fields include goes to Java.

use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, Request, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use dhis_auth::CurrentUser;
use dhis_core_model::jsonb;
use dhis_core_model::locale::Locale;
use dhis_query::field_filter::{Node, filter, include_paths};
use dhis_query::gist::output::Cell;
use dhis_query::gist::plan::ColumnType;
use dhis_schema::hibernate::{self, Entity};
use dhis_schema::registered::{self, RegisteredSchema};
use dhis_schema::{Property, jackson};
use http::{HeaderValue, header};
use serde_json::Value;

use super::metadata_controller::{GIST_TYPES, decode, json_requested, translation_locale};
use crate::http::paths::api_path;
use crate::http::web_message::WebMessage;
use crate::http::{RequestContext, fallback, json_response};
use crate::params::Params;
use crate::{AppState, Authenticated};

pub fn routes() -> Router<Arc<AppState>> {
    let mut r = Router::new();
    for singular in GIST_TYPES {
        let schema = registered::by_singular(singular).expect("registered schema");
        let endpoint = schema
            .relative_api_endpoint
            .as_deref()
            .expect("type with an endpoint");
        r = r.route(
            &format!("/api{endpoint}/{{uid}}"),
            get(
                move |state: State<Arc<AppState>>,
                      user: Authenticated,
                      uid: Path<String>,
                      req: Request| { get_object(state, user, uid, req, schema) },
            ),
        );
    }
    r
}

/// `/api/{type}/{uid}` paths the handler serves (authentication needs the settings for the
/// translation locale).
pub fn is_object_path(normalized_path: &str) -> bool {
    GIST_TYPES.iter().any(|s| {
        registered::by_singular(s)
            .and_then(|schema| schema.relative_api_endpoint.as_deref())
            .is_some_and(|e| {
                normalized_path
                    .strip_prefix("/api")
                    .and_then(|p| p.strip_prefix(e))
                    .and_then(|p| p.strip_prefix('/'))
                    .is_some_and(is_uid_segment)
            })
    })
}

/// The mapping's pattern `{uid:[a-zA-Z0-9]{11}}`.
fn is_uid_segment(s: &str) -> bool {
    s.len() == 11 && s.bytes().all(|b| b.is_ascii_alphanumeric())
}

#[derive(Debug, Clone, Copy)]
struct NotPorted(&'static str);

/// What each written property needs.
enum Source {
    /// A selected column.
    Column(usize),
    Sharing(usize),
    Translations(usize),
    AttributeValues(usize),
    /// `UserPropertyTransformer`: columns of a joined `userinfo` row.
    User(UserCols),
    /// The id of a referenced object (`{"id": ...}`).
    RefId(usize),
    /// `getTranslation(key, base)`.
    Translated {
        key: &'static str,
        base: Base,
    },
    Access,
    Href,
}

enum Base {
    Column(usize),
    /// `getFormNameFallback()`: the form name unless empty, else the display name.
    FormNameFallback {
        form_name: usize,
        name: usize,
    },
}

#[derive(Clone, Copy)]
struct UserCols {
    uid: usize,
    code: usize,
    name: usize,
    first: usize,
    surname: usize,
    username: usize,
    /// `None`: the type has no translations (`User` is not translatable).
    translations: Option<usize>,
}

struct Select {
    columns: Vec<(String, ColumnType)>,
    joins: Vec<String>,
}

impl Select {
    fn add(&mut self, expr: String, ty: ColumnType) -> usize {
        if let Some(i) = self.columns.iter().position(|(e, _)| *e == expr) {
            return i;
        }
        self.columns.push((expr, ty));
        self.columns.len() - 1
    }
}

fn mapped_column(entity: &Entity, field: &str) -> Option<String> {
    let m = entity.properties.get(field)?;
    (m.kind == "property" && !m.formula && m.column_table.is_none())
        .then(|| m.column.clone())
        .flatten()
}

fn column_type(p: &Property, entity: &Entity) -> Result<(String, ColumnType), NotPorted> {
    let field = p.field_name.as_deref().ok_or(NotPorted("no field"))?;
    let m = entity
        .properties
        .get(field)
        .ok_or(NotPorted("property not mapped"))?;
    if m.kind != "property" || m.formula || m.column_table.is_some() {
        return Err(NotPorted("mapping kind"));
    }
    let col = m.column.clone().ok_or(NotPorted("no column"))?;
    if p.property_type.name() == "CONSTANT" {
        return match m.hibernate_type.as_deref() {
            Some("enum-string") => Ok((col, ColumnType::Text)),
            _ => Err(NotPorted("enum mapping")),
        };
    }
    let ty = match p.klass.as_deref() {
        Some("java.lang.String") => ColumnType::Text,
        Some("java.lang.Double" | "double") => ColumnType::Double,
        Some("java.lang.Integer" | "int") => ColumnType::Int,
        Some("java.lang.Long" | "long") => ColumnType::Long,
        Some("java.lang.Boolean" | "boolean") => ColumnType::Bool,
        Some("java.util.Date") => ColumnType::Timestamp,
        _ => return Err(NotPorted("property type")),
    };
    let ok = match m.hibernate_type.as_deref() {
        None => true,
        Some("text" | "string") => ty == ColumnType::Text,
        Some("timestamp" | "date") => ty == ColumnType::Timestamp,
        Some("double") => ty == ColumnType::Double,
        Some("integer") => ty == ColumnType::Int,
        Some("long") => ty == ColumnType::Long,
        Some("boolean") => ty == ColumnType::Bool,
        _ => false,
    };
    if !ok {
        return Err(NotPorted("hibernate type"));
    }
    Ok((format!("e.{col}"), ty))
}

/// The entity's own and joined columns for the written properties that are included.
fn plan_sources(
    schema: &'static RegisteredSchema,
    entity: &'static Entity,
    includes: &std::collections::BTreeSet<String>,
    sel: &mut Select,
) -> Result<Vec<(&'static str, Source)>, NotPorted> {
    let props = jackson::properties(&schema.klass).ok_or(NotPorted("no jackson properties"))?;
    let translations_col = mapped_column(entity, "translations");
    let mut user_joins: Vec<(String, UserCols)> = Vec::new();
    let mut user_ref = |sel: &mut Select, fk: &str| -> Result<UserCols, NotPorted> {
        if let Some((_, c)) = user_joins.iter().find(|(k, _)| k == fk) {
            return Ok(*c);
        }
        let user = hibernate::entity("org.hisp.dhis.user.User").ok_or(NotPorted("user mapping"))?;
        let id = &user.id.as_ref().ok_or(NotPorted("user id"))?.column;
        let alias = format!("u{}", user_joins.len());
        sel.joins.push(format!(
            " left join {} {alias} on {alias}.{id} = e.{fk}",
            user.table.as_deref().ok_or(NotPorted("user table"))?
        ));
        let c = |f: &str| -> Result<String, NotPorted> {
            Ok(format!(
                "{alias}.{}",
                mapped_column(user, f).ok_or(NotPorted("user column"))?
            ))
        };
        let cols = UserCols {
            uid: sel.add(c("uid")?, ColumnType::Text),
            code: sel.add(c("code")?, ColumnType::Text),
            name: sel.add(c("name")?, ColumnType::Text),
            first: sel.add(c("firstName")?, ColumnType::Text),
            surname: sel.add(c("surname")?, ColumnType::Text),
            username: sel.add(c("username")?, ColumnType::Text),
            translations: mapped_column(user, "translations")
                .map(|t| sel.add(format!("{alias}.{t}::text"), ColumnType::Text)),
        };
        user_joins.push((fk.to_string(), cols));
        Ok(cols)
    };
    let created_by_fk = entity
        .properties
        .get("createdBy")
        .filter(|m| m.kind == "many-to-one")
        .and_then(|m| m.column.clone());

    let mut out = Vec::new();
    for jp in props {
        let name = jp.name.as_str();
        if !includes.contains(name) {
            continue;
        }
        // nested selections below a property only work where handled
        let nested: Vec<&String> = includes
            .iter()
            .filter(|i| i.starts_with(name) && i.as_bytes().get(name.len()) == Some(&b'.'))
            .collect();
        let key: &'static str = &jp.name;
        let translate = |sel: &mut Select, tkey: &'static str, base: &str| {
            let col = mapped_column(entity, base).ok_or(NotPorted("translation base"))?;
            sel.add(
                format!(
                    "e.{}",
                    translations_col
                        .as_deref()
                        .ok_or(NotPorted("no translations"))?
                ) + "::text",
                ColumnType::Text,
            );
            Ok::<_, NotPorted>(Source::Translated {
                key: tkey,
                base: Base::Column(sel.add(format!("e.{col}"), ColumnType::Text)),
            })
        };
        let src = match name {
            "displayName" => translate(sel, "NAME", "name")?,
            "displayShortName" => translate(sel, "SHORT_NAME", "shortName")?,
            "displayDescription" => translate(sel, "DESCRIPTION", "description")?,
            "displayFormName" if mapped_column(entity, "formName").is_none() => {
                // no form name: getFormNameFallback() is the display name
                let n = mapped_column(entity, "name").ok_or(NotPorted("name"))?;
                sel.add(
                    format!(
                        "e.{}::text",
                        translations_col.as_deref().ok_or(NotPorted("tr"))?
                    ),
                    ColumnType::Text,
                );
                let name = sel.add(format!("e.{n}"), ColumnType::Text);
                Source::Translated {
                    key: "FORM_NAME",
                    base: Base::FormNameFallback {
                        form_name: usize::MAX,
                        name,
                    },
                }
            }
            "displayFormName" => {
                let form = mapped_column(entity, "formName").ok_or(NotPorted("form name"))?;
                let n = mapped_column(entity, "name").ok_or(NotPorted("name"))?;
                sel.add(
                    format!(
                        "e.{}::text",
                        translations_col.as_deref().ok_or(NotPorted("tr"))?
                    ),
                    ColumnType::Text,
                );
                Source::Translated {
                    key: "FORM_NAME",
                    base: Base::FormNameFallback {
                        form_name: sel.add(format!("e.{form}"), ColumnType::Text),
                        name: sel.add(format!("e.{n}"), ColumnType::Text),
                    },
                }
            }
            "user" => {
                let fk = created_by_fk.as_deref().ok_or(NotPorted("createdBy"))?;
                Source::User(user_ref(sel, fk)?)
            }
            "access" => {
                let col = mapped_column(entity, "sharing").ok_or(NotPorted("sharing"))?;
                sel.add(format!("e.{col}::text"), ColumnType::Text);
                Source::Access
            }
            "href" => Source::Href,
            // BaseNameableObject#getFormName returns a field that types without a mapped form
            // name (e.g. Constant) never set: always null, so never written
            "formName" if mapped_column(entity, "formName").is_none() => continue,
            _ => {
                let p = schema.property(name).ok_or(NotPorted("unknown property"))?;
                if !p.persisted {
                    return Err(NotPorted("computed property"));
                }
                let field = p.field_name.as_deref().ok_or(NotPorted("no field"))?;
                let m = entity
                    .properties
                    .get(field)
                    .ok_or(NotPorted("property not mapped"))?;
                match (m.kind.as_str(), m.hibernate_type.as_deref()) {
                    ("property", Some("jsbObjectSharing")) => {
                        let col = m.column.as_deref().ok_or(NotPorted("col"))?;
                        Source::Sharing(sel.add(format!("e.{col}::text"), ColumnType::Text))
                    }
                    ("property", Some("jblTranslations")) => {
                        let col = m.column.as_deref().ok_or(NotPorted("col"))?;
                        Source::Translations(sel.add(format!("e.{col}::text"), ColumnType::Text))
                    }
                    ("property", Some("jsbAttributeValues")) => {
                        let col = m.column.as_deref().ok_or(NotPorted("col"))?;
                        Source::AttributeValues(sel.add(format!("e.{col}::text"), ColumnType::Text))
                    }
                    ("many-to-one", _) if m.column_table.is_none() => {
                        let fk = m.column.as_deref().ok_or(NotPorted("fk"))?;
                        if p.property_transformer
                            && p.klass.as_deref() == Some("org.hisp.dhis.user.User")
                        {
                            Source::User(user_ref(sel, fk)?)
                        } else {
                            if nested.iter().any(|n| *n != &format!("{name}.id")) {
                                return Err(NotPorted("nested reference fields"));
                            }
                            let target = m
                                .class
                                .as_deref()
                                .and_then(hibernate::entity)
                                .filter(|t| t.is_plain_class())
                                .ok_or(NotPorted("reference target"))?;
                            let uid =
                                mapped_column(target, "uid").ok_or(NotPorted("target uid"))?;
                            let tid = &target.id.as_ref().ok_or(NotPorted("target id"))?.column;
                            let table = target.table.as_deref().ok_or(NotPorted("table"))?;
                            Source::RefId(sel.add(
                                format!("(select t.{uid} from {table} t where t.{tid} = e.{fk})"),
                                ColumnType::Text,
                            ))
                        }
                    }
                    ("property", _) => {
                        let (expr, ty) = column_type(p, entity)?;
                        Source::Column(sel.add(super_select(&expr, ty), ty))
                    }
                    _ => return Err(NotPorted("property kind")),
                }
            }
        };
        out.push((key, src));
    }
    Ok(out)
}

fn super_select(expr: &str, ty: ColumnType) -> String {
    match ty {
        ColumnType::Timestamp => format!("to_char({expr}, 'YYYY-MM-DD HH24:MI:SS.US')"),
        _ => expr.to_string(),
    }
}

fn text(cells: &[Cell], i: usize) -> Option<&str> {
    match &cells[i] {
        Cell::Text(s) => Some(s),
        _ => None,
    }
}

fn s(v: Option<&str>) -> Node {
    v.map_or(Node::Scalar(Value::Null), |v| {
        Node::Scalar(Value::String(v.into()))
    })
}

struct RenderCtx<'a> {
    schema: &'static RegisteredSchema,
    locale: Locale,
    href: String,
    acl_user: dhis_acl::User<'a>,
    /// Display names of users and groups named in sharing.
    display_names: &'a std::collections::HashMap<String, Option<String>>,
}

fn sharing_node(text: &str, ctx: &RenderCtx<'_>, with_names: bool) -> Option<Node> {
    let v = jsonb::Ordered::parse(text)?;
    let jsonb::Ordered::Object(_) = &v else {
        return None;
    };
    let string = |k: &str| match v.get(k) {
        None | Some(jsonb::Ordered::Null) => Some(None),
        Some(jsonb::Ordered::String(s)) => Some(Some(s.clone())),
        _ => None,
    };
    let mut props: Vec<(String, Node)> = Vec::new();
    if let Some(owner) = string("owner")? {
        props.push(("owner".into(), Node::Scalar(Value::String(owner))));
    }
    for name in ["users", "userGroups"] {
        let mut entries = Vec::new();
        if let Some(jsonb::Ordered::Object(m)) = v.get(name) {
            for (k, a) in m {
                let field = |f: &str| match a.get(f) {
                    None | Some(jsonb::Ordered::Null) => Some(None),
                    Some(jsonb::Ordered::String(s)) => Some(Some(s.clone())),
                    _ => None,
                };
                let id = field("id")?;
                let mut ap: Vec<(String, Node)> = Vec::new();
                if with_names
                    && let Some(n) = id
                        .as_ref()
                        .and_then(|i| ctx.display_names.get(i))
                        .cloned()
                        .flatten()
                {
                    ap.push(("displayName".into(), Node::Scalar(Value::String(n))));
                }
                if let Some(acc) = field("access")? {
                    ap.push(("access".into(), Node::Scalar(Value::String(acc))));
                }
                if let Some(id) = id {
                    ap.push(("id".into(), Node::Scalar(Value::String(id))));
                }
                let klass = if name == "users" {
                    "org.hisp.dhis.user.sharing.UserAccess"
                } else {
                    "org.hisp.dhis.user.sharing.UserGroupAccess"
                };
                entries.push((k.clone(), Node::Bean { klass, props: ap }));
            }
        } else if !matches!(v.get(name), None | Some(jsonb::Ordered::Null)) {
            return None;
        }
        props.push((name.to_string(), Node::Map(entries)));
    }
    if let Some(public) = string("public")? {
        props.push(("public".into(), Node::Scalar(Value::String(public))));
    }
    Some(Node::Bean {
        klass: "org.hisp.dhis.user.sharing.Sharing",
        props,
    })
}

fn acl_sharing(text: Option<&str>) -> Option<dhis_acl::Sharing> {
    let Some(text) = text else {
        return Some(dhis_acl::Sharing::default());
    };
    let v = jsonb::Ordered::parse(text)?;
    let str_of = |o: Option<&jsonb::Ordered>| o.and_then(|x| x.as_str()).map(str::to_string);
    let entries = |name: &str| -> Vec<(String, Option<String>)> {
        match v.get(name) {
            Some(jsonb::Ordered::Object(m)) => m
                .iter()
                .filter_map(|(_, a)| Some((str_of(a.get("id"))?, str_of(a.get("access")))))
                .collect(),
            _ => Vec::new(),
        }
    };
    Some(dhis_acl::Sharing {
        owner: str_of(v.get("owner")),
        public: str_of(v.get("public")),
        users: entries("users"),
        user_groups: entries("userGroups"),
    })
}

/// `UserPropertyTransformer.JacksonSerialize`: always the five fields, nulls written.
fn user_node(cells: &[Cell], c: UserCols, locale: &Locale) -> Option<Node> {
    let Some(uid) = text(cells, c.uid) else {
        return Some(Node::Scalar(Value::Null));
    };
    // User#getName(): the name, or firstName + " " + surname for transient users
    let name = match text(cells, c.name) {
        Some(n) => n.to_string(),
        None => format!(
            "{} {}",
            text(cells, c.first).unwrap_or("null"),
            text(cells, c.surname).unwrap_or("null")
        ),
    };
    let display = jsonb::translate(
        c.translations.and_then(|i| text(cells, i)),
        locale,
        "NAME",
        Some(&name),
    )?;
    Some(Node::Map(vec![
        ("id".into(), s(Some(uid))),
        ("code".into(), s(text(cells, c.code))),
        ("name".into(), s(Some(&name))),
        ("displayName".into(), s(display.as_deref())),
        ("username".into(), s(text(cells, c.username))),
    ]))
}

fn render(
    sources: &[(&'static str, Source)],
    cells: &[Cell],
    ctx: &RenderCtx<'_>,
    sharing_col: Option<usize>,
    translations_col: Option<usize>,
) -> Result<Node, NotPorted> {
    let mut props: Vec<(String, Node)> = Vec::new();
    let bad = NotPorted("stored value not reproduced");
    for (name, src) in sources {
        let node = match src {
            Source::Column(i) => match &cells[*i] {
                Cell::Null => continue,
                Cell::Text(t) => Node::Scalar(Value::String(t.clone())),
                Cell::Double(d) => Node::Scalar(
                    serde_json::Number::from_f64(*d)
                        .map(Value::Number)
                        .ok_or(NotPorted("non-finite double"))?,
                ),
                Cell::Int(v) => Node::Scalar(Value::from(*v)),
                Cell::Bool(b) => Node::Scalar(Value::Bool(*b)),
                _ => return Err(bad),
            },
            Source::Sharing(i) => match text(cells, *i) {
                None => continue,
                Some(t) => sharing_node(t, ctx, true).ok_or(bad)?,
            },
            Source::Translations(i) => {
                let set = match text(cells, *i) {
                    None => continue,
                    Some(t) => jsonb::translations(t).ok_or(bad)?,
                };
                Node::Array(
                    set.into_iter()
                        .map(|t| {
                            let mut tp: Vec<(String, Node)> = Vec::new();
                            if let Some(l) = t.locale {
                                tp.push((
                                    "locale".into(),
                                    Node::Scalar(Value::String(l.to_string())),
                                ));
                            }
                            if let Some(p) = t.property {
                                tp.push(("property".into(), Node::Scalar(Value::String(p))));
                            }
                            if let Some(v) = t.value {
                                tp.push(("value".into(), Node::Scalar(Value::String(v))));
                            }
                            Node::Bean {
                                klass: "org.hisp.dhis.translation.Translation",
                                props: tp,
                            }
                        })
                        .collect(),
                )
            }
            Source::AttributeValues(i) => {
                // AttributeValuesSerializer: the array form, written raw (not filtered)
                let values = match text(cells, *i) {
                    None => continue,
                    Some(t) => jsonb::attribute_values(t).ok_or(bad)?,
                };
                Node::Array(
                    values
                        .into_iter()
                        .map(|(k, v)| {
                            Node::Map(vec![
                                ("value".into(), Node::Scalar(Value::String(v))),
                                (
                                    "attribute".into(),
                                    Node::Map(vec![("id".into(), Node::Scalar(Value::String(k)))]),
                                ),
                            ])
                        })
                        .collect(),
                )
            }
            Source::User(c) => match user_node(cells, *c, &ctx.locale).ok_or(bad)? {
                Node::Scalar(Value::Null) => continue,
                n => n,
            },
            Source::RefId(i) => match text(cells, *i) {
                None => continue,
                Some(uid) => Node::Bean {
                    klass: "",
                    props: vec![("id".into(), Node::Scalar(Value::String(uid.into())))],
                },
            },
            Source::Translated { key, base } => {
                let tr = translations_col.and_then(|i| text(cells, i));
                let default = match base {
                    Base::Column(i) => text(cells, *i).map(str::to_string),
                    Base::FormNameFallback { form_name, name } => {
                        let form = (*form_name != usize::MAX)
                            .then(|| text(cells, *form_name))
                            .flatten();
                        match form.filter(|f| !f.is_empty()) {
                            Some(f) => Some(f.to_string()),
                            None => jsonb::translate(tr, &ctx.locale, "NAME", text(cells, *name))
                                .ok_or(bad)?,
                        }
                    }
                };
                match jsonb::translate(tr, &ctx.locale, key, default.as_deref()).ok_or(bad)? {
                    None => continue,
                    Some(v) => Node::Scalar(Value::String(v)),
                }
            }
            Source::Access => {
                let sharing = acl_sharing(sharing_col.and_then(|i| text(cells, i))).ok_or(bad)?;
                let a = dhis_acl::get_access(&ctx.acl_user, ctx.schema, &sharing)
                    .ok_or(NotPorted("access of this type"))?;
                let mut ap: Vec<(String, Node)> = vec![
                    ("manage".into(), Node::Scalar(Value::Bool(a.manage))),
                    ("write".into(), Node::Scalar(Value::Bool(a.write))),
                    ("read".into(), Node::Scalar(Value::Bool(a.read))),
                    ("update".into(), Node::Scalar(Value::Bool(a.update))),
                    ("delete".into(), Node::Scalar(Value::Bool(a.delete))),
                ];
                if let Some((read, write)) = a.data {
                    ap.push((
                        "data".into(),
                        Node::Bean {
                            klass: "org.hisp.dhis.security.acl.AccessData",
                            props: vec![
                                ("write".into(), Node::Scalar(Value::Bool(write))),
                                ("read".into(), Node::Scalar(Value::Bool(read))),
                            ],
                        },
                    ));
                }
                Node::Bean {
                    klass: "org.hisp.dhis.security.acl.Access",
                    props: ap,
                }
            }
            Source::Href => Node::Scalar(Value::String(ctx.href.clone())),
        };
        props.push((name.to_string(), node));
    }
    Ok(Node::Bean {
        klass: ctx.schema.klass.as_str(),
        props,
    })
}

/// `getObject`.
async fn get_object(
    State(state): State<Arc<AppState>>,
    Authenticated(user): Authenticated,
    Path(uid): Path<String>,
    req: Request,
    schema: &'static RegisteredSchema,
) -> Response {
    if !is_uid_segment(&uid) {
        return fallback("not an object id");
    }
    if !json_requested(&req) {
        return fallback("content negotiation");
    }
    let parts = RequestParts {
        query: req.uri().query().map(str::to_string),
        headers: req.headers().clone(),
        original_path: req
            .extensions()
            .get::<RequestContext>()
            .map_or_else(|| req.uri().path().to_string(), |c| c.original_path.clone()),
    };
    drop(req);
    match object(&state, &user, &uid, &parts, schema).await {
        Ok(r) => r,
        Err(NotPorted(reason)) => fallback(reason),
    }
}

/// What the handler needs of the request (owned, so the future stays `Send`).
struct RequestParts {
    query: Option<String>,
    headers: http::HeaderMap,
    original_path: String,
}

async fn object(
    state: &AppState,
    user: &CurrentUser,
    uid: &str,
    req: &RequestParts,
    schema: &'static RegisteredSchema,
) -> Result<Response, NotPorted> {
    let db = state.db.as_ref().ok_or(NotPorted("no database"))?;
    let params = Params::parse(req.query.as_deref());
    match params.all("defaults").as_slice() {
        [] | ["INCLUDE"] => {}
        _ => return Err(NotPorted("defaults parameter")),
    }
    let authorities = user
        .authorities
        .as_ref()
        .ok_or(NotPorted("no authorities"))?;
    let superuser = user.is_super() == Some(true);
    let identity = user.identity.as_ref();
    if !superuser && identity.is_none() {
        return Err(NotPorted("session identity unknown"));
    }
    let empty = Vec::new();
    let acl_user = dhis_acl::User {
        superuser,
        uid: identity.map_or("", |i| i.uid.as_str()),
        group_ids: identity.map_or(&empty, |i| &i.group_ids),
        authorities,
    };
    if !dhis_acl::can_read_type(&acl_user, schema) {
        return Err(NotPorted("type not readable"));
    }
    let locale_text = translation_locale(user, "").ok_or(NotPorted("locale"))?;
    let locale = Locale::parse(&locale_text).map_err(|_| NotPorted("locale"))?;

    // getFieldsObject(): `*` unless given
    let fields = {
        let all = params.all("fields");
        if all.is_empty() || all.iter().all(|f| f.is_empty()) {
            "*".to_string()
        } else {
            all.join(",")
        }
    };
    let paths = dhis_query::fields::parse(&fields).map_err(|_| NotPorted("fields"))?;
    let includes = include_paths(&paths, &schema.klass).map_err(|_| NotPorted("transformers"))?;

    let entity = hibernate::entity(&schema.klass).ok_or(NotPorted("not mapped"))?;
    if !entity.is_plain_class() {
        return Err(NotPorted("mapping shape"));
    }
    let table = entity.table.as_deref().ok_or(NotPorted("table"))?;
    let uid_col = mapped_column(entity, "uid").ok_or(NotPorted("uid"))?;
    let mut sel = Select {
        columns: Vec::new(),
        joins: Vec::new(),
    };
    let sources = plan_sources(schema, entity, &includes, &mut sel)?;
    let sharing_col =
        mapped_column(entity, "sharing").map(|c| sel.add(format!("e.{c}::text"), ColumnType::Text));
    let translations_col = mapped_column(entity, "translations")
        .map(|c| sel.add(format!("e.{c}::text"), ColumnType::Text));
    let uid_index = sel.add(format!("e.{uid_col}"), ColumnType::Text);

    // manager.get: the object, if the user can read it (sharing)
    let access_sql = if superuser || schema.property("sharing").is_none_or(|p| !p.persisted) {
        "1=1".to_string()
    } else {
        let uid = identity.map_or("", |i| i.uid.as_str());
        dhis_query::gist::plan::sharing_condition(
            "e",
            entity,
            uid,
            identity.map_or(&[][..], |i| &i.group_ids),
        )
        .ok_or(NotPorted("sharing filter"))?
    };
    let select_list: Vec<&str> = sel.columns.iter().map(|(e, _)| e.as_str()).collect();
    let sql = format!(
        "select {} from {table} e{} where e.{uid_col} = $1 and ({access_sql})",
        select_list.join(", "),
        sel.joins.concat()
    );
    let row = sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(uid)
        .fetch_optional(db)
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, "metadata object query failed");
            NotPorted("query failed")
        })?;
    let Some(row) = row else {
        // getEntity: NotFoundException(type, uid)
        return Ok(WebMessage::not_found(format!(
            "{} with id {uid} could not be found.",
            schema.simple_name()
        ))
        .into_response());
    };
    let mut cells = Vec::with_capacity(sel.columns.len());
    for (i, (_, ty)) in sel.columns.iter().enumerate() {
        cells.push(decode(&row, i, *ty).ok_or(NotPorted("column type"))?);
    }

    // applySharingDisplayNames: only when `sharing` is written
    let mut display_names = std::collections::HashMap::new();
    if includes.contains("sharing")
        && let Some(text) = sharing_col.and_then(|i| self::text(&cells, i))
    {
        let sharing = acl_sharing(Some(text)).ok_or(NotPorted("sharing"))?;
        for (id, _) in &sharing.users {
            let n = user_display_name(db, id).await?;
            display_names.insert(id.clone(), n);
        }
        for (id, _) in &sharing.user_groups {
            let n = group_display_name(db, id, &locale).await?;
            display_names.insert(id.clone(), n);
        }
    }
    let href = format!(
        "{}{}/{}",
        api_path(&req.headers, &req.original_path),
        schema.relative_api_endpoint.as_deref().unwrap_or(""),
        text(&cells, uid_index).unwrap_or(uid)
    );
    let ctx = RenderCtx {
        schema,
        locale,
        href,
        acl_user,
        display_names: &display_names,
    };
    let node = render(&sources, &cells, &ctx, sharing_col, translations_col)?;
    let out = filter(&node, &includes);
    let mut r = json_response(out.to_json_string());
    r.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-cache, private"),
    );
    Ok(r)
}

/// `UserStore#getDisplayName`: `concat(firstname, ' ', surname)`.
async fn user_display_name(db: &sqlx::PgPool, uid: &str) -> Result<Option<String>, NotPorted> {
    sqlx::query_scalar::<_, Option<String>>(
        "select concat(firstname, ' ', surname) from userinfo where uid = $1",
    )
    .bind(uid)
    .fetch_optional(db)
    .await
    .map(Option::flatten)
    .map_err(|_| NotPorted("user display name"))
}

/// `UserGroupService#getDisplayName`: the group's display name (translated); a missing group
/// fails in Java.
async fn group_display_name(
    db: &sqlx::PgPool,
    uid: &str,
    locale: &Locale,
) -> Result<Option<String>, NotPorted> {
    let row: Option<(Option<String>, Option<String>)> =
        sqlx::query_as("select name, translations::text from usergroup where uid = $1")
            .bind(uid)
            .fetch_optional(db)
            .await
            .map_err(|_| NotPorted("group display name"))?;
    let (name, tr) = row.ok_or(NotPorted("missing group"))?;
    jsonb::translate(tr.as_deref(), locale, "NAME", name.as_deref())
        .ok_or(NotPorted("group translations"))
}
