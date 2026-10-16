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

pub(crate) mod list;

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
pub(crate) struct NotPorted(pub &'static str);

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
    /// A collection selected as a JSON array (`json_agg`).
    Collection {
        index: usize,
        kind: CollectionKind,
    },
    /// A fixed value (constant getters, defaults of unmapped fields).
    Fixed(Node),
    /// A collection only reproduced when empty (its row count); otherwise the request goes to Java.
    EmptyOnly(usize, &'static str),
    /// A text value computed in SQL; `?` marks a value not reproducible here.
    Unanimous(usize),
    /// A value that is only reproduced when null (left out); otherwise the request goes to Java.
    NullOnly(usize),
    /// `getUid()` (`getDimensionItem()`).
    Uid(usize),
    /// A boolean computed in SQL.
    Bool(usize),
    /// `isDefault()`: `"default".equals(name)`.
    IsDefault(usize),
    /// `getLegendSet()`: the first of the legend sets, as a reference.
    FirstRef(usize),
}

#[derive(Clone, Copy)]
enum CollectionKind {
    /// Identifiable items: `[{"id": ...}]`.
    Refs,
    /// Users through `UserPropertyTransformer`.
    Users,
    /// `[{"id": ...}]` written whatever the field filter says (no schema for the item type).
    IdMaps,
    /// Plain values (`Set<String>` iterates in hash order).
    Values { string_set: bool },
}

enum Base {
    Column(usize),
    Null,
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
            sel.add(
                format!(
                    "e.{}",
                    translations_col
                        .as_deref()
                        .ok_or(NotPorted("no translations"))?
                ) + "::text",
                ColumnType::Text,
            );
            // an unmapped base field (e.g. Option#shortName) is null
            let base = match mapped_column(entity, base) {
                Some(col) => Base::Column(sel.add(format!("e.{col}"), ColumnType::Text)),
                None if schema.property(base).is_some_and(|p| !p.persisted) => Base::Null,
                None => return Err(NotPorted("translation base")),
            };
            Ok::<_, NotPorted>(Source::Translated { key: tkey, base })
        };
        if let Some(o) = class_override(schema, entity, name, sel, includes)? {
            if let Some(src) = o {
                out.push((key, src));
            }
            continue;
        }
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
            "user" => match created_by_fk.as_deref() {
                Some(fk) => Source::User(user_ref(sel, fk)?),
                // the field is not mapped for this type: always null
                None => continue,
            },
            "dimensionItem" => Source::Uid(sel.add(
                format!(
                    "e.{}",
                    mapped_column(entity, "uid").ok_or(NotPorted("uid"))?
                ),
                ColumnType::Text,
            )),
            "dimensionItemType" => Source::Fixed(Node::Scalar(Value::String(
                dimension_item_type(&schema.klass)
                    .ok_or(NotPorted("dimension item type"))?
                    .into(),
            ))),
            "queryMods" => continue,
            "optionSetValue" => {
                let fk = entity
                    .properties
                    .get("optionSet")
                    .filter(|m| m.kind == "many-to-one")
                    .and_then(|m| m.column.as_deref())
                    .ok_or(NotPorted("optionSet"))?;
                Source::Bool(sel.add(format!("e.{fk} is not null"), ColumnType::Bool))
            }
            "isDefault" if DEFAULT_NAMED.contains(&schema.klass.as_str()) => {
                Source::IsDefault(sel.add(
                    format!(
                        "e.{}",
                        mapped_column(entity, "name").ok_or(NotPorted("name"))?
                    ),
                    ColumnType::Text,
                ))
            }
            "legendSet" if schema.property("legendSets").is_some_and(|p| p.persisted) => {
                let p = schema
                    .property("legendSets")
                    .ok_or(NotPorted("legendSets"))?;
                let (expr, _) = collection_sql(entity, p)?;
                Source::FirstRef(sel.add(expr, ColumnType::Text))
            }
            "access" => {
                // without mapped sharing the object keeps `new Sharing()`
                if let Some(col) = mapped_column(entity, "sharing") {
                    sel.add(format!("e.{col}::text"), ColumnType::Text);
                }
                Source::Access
            }
            "href" => Source::Href,
            // BaseNameableObject#getFormName returns a field that types without a mapped form
            // name (e.g. Constant) never set: always null, so never written
            "formName" if mapped_column(entity, "formName").is_none() => continue,
            _ => {
                let p = schema.property(name).ok_or(NotPorted("unknown property"))?;
                if !p.persisted {
                    // inherited fields the type does not map keep their initial value
                    if p.field_name
                        .as_deref()
                        .is_none_or(|f| !entity.properties.contains_key(f))
                    {
                        match name {
                            "sharing" => {
                                out.push((key, Source::Fixed(empty_sharing())));
                                continue;
                            }
                            "attributeValues" | "translations" => {
                                out.push((key, Source::Fixed(Node::Array(vec![]))));
                                continue;
                            }
                            "createdBy" | "lastUpdatedBy" | "shortName" | "code"
                            | "description" => {
                                continue;
                            }
                            _ => {}
                        }
                    }
                    tracing::debug!(class = %schema.klass, property = name, "computed property not ported");
                    return Err(NotPorted("computed property"));
                }
                if p.collection
                    && !matches!(
                        entity
                            .properties
                            .get(p.field_name.as_deref().unwrap_or(""))
                            .and_then(|m| m.hibernate_type.as_deref()),
                        Some("jblTranslations")
                    )
                {
                    let plan = if nested.iter().any(|n| *n != &format!("{name}.id")) {
                        Err(NotPorted("nested collection fields"))
                    } else {
                        collection_sql(entity, p)
                    };
                    let (expr, kind) = match plan {
                        Ok(x) => x,
                        // still reproducible when empty: `[]`
                        Err(e) => match collection_count_sql(entity, p) {
                            Some(count) => {
                                out.push((
                                    key,
                                    Source::EmptyOnly(sel.add(count, ColumnType::Long), e.0),
                                ));
                                continue;
                            }
                            None => return Err(e),
                        },
                    };
                    out.push((
                        key,
                        Source::Collection {
                            index: sel.add(expr, ColumnType::Text),
                            kind,
                        },
                    ));
                    continue;
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
                    // geometries (GeoJSON through Jackson) not reproduced yet: only absence
                    ("property", None)
                        if p.klass.as_deref() == Some("org.locationtech.jts.geom.Geometry") =>
                    {
                        let col = m.column.as_deref().ok_or(NotPorted("col"))?;
                        Source::NullOnly(sel.add(
                            format!("case when e.{col} is null then null else 'x' end"),
                            ColumnType::Text,
                        ))
                    }
                    // JSON values not reproduced yet: only their absence is
                    ("property", Some("jbObjectStyle" | "jbValueTypeOptions")) => {
                        let col = m.column.as_deref().ok_or(NotPorted("col"))?;
                        Source::NullOnly(sel.add(format!("e.{col}::text"), ColumnType::Text))
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

/// Getters that particular classes override (read from their Java source). `Some(None)`: the
/// value is always null (not written); `None`: no override.
#[allow(clippy::type_complexity)]
fn class_override(
    schema: &RegisteredSchema,
    entity: &Entity,
    name: &str,
    sel: &mut Select,
    includes: &std::collections::BTreeSet<String>,
) -> Result<Option<Option<Source>>, NotPorted> {
    let simple = schema.simple_name();
    Ok(Some(match (simple, name) {
        // CategoryOption: no aggregation type unless query modifiers (never set here)
        ("CategoryOption", "aggregationType" | "totalAggregationType") => None,
        ("CategoryOption", "hasAggregationType") => {
            Some(Source::Fixed(Node::Scalar(Value::Bool(false))))
        }
        // BaseDimensionalItemObject fields these types do not map: initial values
        ("DataElementGroup" | "OrganisationUnitGroup" | "OrganisationUnit", "legendSets") => {
            Some(Source::Fixed(Node::Array(vec![])))
        }
        ("Indicator" | "OrganisationUnitGroup" | "OrganisationUnit", "aggregationType") => None,
        // Indicator: transient values, only set while exploding expressions
        ("Indicator", "explodedNumerator" | "explodedDenominator") => None,
        ("Indicator", "displayNumeratorDescription" | "displayDenominatorDescription") => {
            let (key, base) = if name == "displayNumeratorDescription" {
                ("NUMERATOR_DESCRIPTION", "numeratorDescription")
            } else {
                ("DENOMINATOR_DESCRIPTION", "denominatorDescription")
            };
            let tr = mapped_column(entity, "translations").ok_or(NotPorted("translations"))?;
            sel.add(format!("e.{tr}::text"), ColumnType::Text);
            let col = mapped_column(entity, base).ok_or(NotPorted("description"))?;
            Some(Source::Translated {
                key,
                base: Base::Column(sel.add(format!("e.{col}"), ColumnType::Text)),
            })
        }
        // getFeatureType(): from the geometry, null without one
        ("OrganisationUnitGroup" | "OrganisationUnit", "featureType") => {
            let col = mapped_column(entity, "geometry").ok_or(NotPorted("geometry"))?;
            Some(Source::NullOnly(sel.add(
                format!("case when e.{col} is null then null else 'x' end"),
                ColumnType::Text,
            )))
        }
        // the first of an always-empty legend set list
        ("DataElementGroup" | "OrganisationUnitGroup" | "OrganisationUnit", "legendSet") => None,
        // OrganisationUnit: transient values set by some API calls only
        ("OrganisationUnit", "type" | "memberCount") => None,
        // OrganisationUnit#isLeaf(): no children
        ("OrganisationUnit", "leaf") => {
            let parent = entity
                .properties
                .get("parent")
                .and_then(|m| m.column.clone())
                .ok_or(NotPorted("parent"))?;
            let table = entity.table.as_deref().ok_or(NotPorted("table"))?;
            let id = &entity.id.as_ref().ok_or(NotPorted("id"))?.column;
            Some(Source::Bool(sel.add(
                format!("not exists (select 1 from {table} c where c.{parent} = e.{id})"),
                ColumnType::Bool,
            )))
        }
        // OrganisationUnit#getAncestors(): parents from the root down
        ("OrganisationUnit", "ancestors") => {
            let parent = entity
                .properties
                .get("parent")
                .and_then(|m| m.column.clone())
                .ok_or(NotPorted("parent"))?;
            let table = entity.table.as_deref().ok_or(NotPorted("table"))?;
            let id = &entity.id.as_ref().ok_or(NotPorted("id"))?.column;
            let uid = mapped_column(entity, "uid").ok_or(NotPorted("uid"))?;
            Some(Source::Collection {
                index: sel.add(
                    format!(
                        "(with recursive a(id, uid, depth) as (select p.{id}, p.{uid}, 1 from {table} p \
                         where p.{id} = e.{parent} union all select p.{id}, p.{uid}, a.depth + 1 \
                         from {table} p join {table} c on p.{id} = c.{parent} join a on c.{id} = a.id \
                         where a.depth < 100) select json_agg(uid order by depth desc) from a)::text"
                    ),
                    ColumnType::Text,
                ),
                kind: CollectionKind::Refs,
            })
        }
        // Category: BaseDimensionalObject values that only analytics requests set
        (
            "Category",
            "filter"
            | "valueType"
            | "optionSet"
            | "aggregationType"
            | "legendSet"
            | "programStage"
            | "program"
            | "dimensionItemKeywords"
            | "repetition"
            | "dimensionName"
            | "dimensionDisplayName",
        ) => None,
        ("Category", "dimension") => Some(Source::Uid(sel.add(
            format!(
                "e.{}",
                mapped_column(entity, "uid").ok_or(NotPorted("uid"))?
            ),
            ColumnType::Text,
        ))),
        ("Category", "allItems") => Some(Source::Fixed(Node::Scalar(Value::Bool(false)))),
        // Category#getItems(): the category options, in list order
        // (written as `{"id": ...}`: Java's default expansion; other sub-fields go to Java)
        ("Category", "items") => {
            if includes
                .iter()
                .any(|i| i.starts_with("items.") && i != "items.id")
            {
                return Err(NotPorted("item fields"));
            }
            let options = schema
                .property("categoryOptions")
                .ok_or(NotPorted("categoryOptions"))?;
            let (expr, _) = collection_sql(entity, options)?;
            Some(Source::Collection {
                index: sel.add(expr, ColumnType::Text),
                kind: CollectionKind::IdMaps,
            })
        }
        // DataElementGroup#getAggregationType(): the first member's (set order); reproduced when
        // the members agree, or there are none
        ("DataElementGroup", "aggregationType") => {
            let m = entity
                .properties
                .get("members")
                .ok_or(NotPorted("members"))?;
            let (t, k, el) = (
                m.table.as_deref().ok_or(NotPorted("members"))?,
                m.key_column.as_deref().ok_or(NotPorted("members"))?,
                m.element_column.as_deref().ok_or(NotPorted("members"))?,
            );
            let id = &entity.id.as_ref().ok_or(NotPorted("id"))?.column;
            Some(Source::Unanimous(sel.add(
                format!(
                    "(select case when count(*) = 0 then null when count(distinct d.aggregationtype) = 1 \
                     then min(d.aggregationtype) else '?' end from dataelement d join {t} g on \
                     d.dataelementid = g.{el} where g.{k} = e.{id})"
                ),
                ColumnType::Text,
            )))
        }
        // UserRole#getUsers(): the members
        ("UserRole", "users") => {
            // the mapped field behind getUsers() is `members`
            let mut p = schema.property("users").ok_or(NotPorted("users"))?.clone();
            p.field_name = Some("members".into());
            p.property_transformer = true;
            let (expr, kind) = collection_sql(entity, &p)?;
            Some(Source::Collection {
                index: sel.add(expr, ColumnType::Text),
                kind,
            })
        }
        _ => return Ok(None),
    }))
}

/// Types whose `isDefault()` is `"default".equals(name)`.
const DEFAULT_NAMED: &[&str] = &[
    "org.hisp.dhis.category.Category",
    "org.hisp.dhis.category.CategoryCombo",
    "org.hisp.dhis.category.CategoryOption",
    "org.hisp.dhis.category.CategoryOptionCombo",
];

/// `getDimensionItemType()` of the types that return a constant.
fn dimension_item_type(klass: &str) -> Option<&'static str> {
    Some(match klass {
        "org.hisp.dhis.dataelement.DataElement" => "DATA_ELEMENT",
        "org.hisp.dhis.dataelement.DataElementGroup" => "DATA_ELEMENT_GROUP",
        "org.hisp.dhis.category.CategoryOption" => "CATEGORY_OPTION",
        "org.hisp.dhis.category.CategoryOptionGroup" => "CATEGORY_OPTION_GROUP",
        "org.hisp.dhis.indicator.Indicator" => "INDICATOR",
        "org.hisp.dhis.organisationunit.OrganisationUnit" => "ORGANISATION_UNIT",
        "org.hisp.dhis.organisationunit.OrganisationUnitGroup" => "ORGANISATION_UNIT_GROUP",
        _ => return None,
    })
}

/// `new Sharing()` as written: no owner/public (null), empty maps.
fn empty_sharing() -> Node {
    Node::Bean {
        klass: "org.hisp.dhis.user.sharing.Sharing",
        props: vec![
            ("users".into(), Node::Map(vec![])),
            ("userGroups".into(), Node::Map(vec![])),
        ],
    }
}

/// A persistent collection as `(select json_agg(...) ...)::text` and how to render it. Lists
/// follow their index column or `@OrderBy`; sets have no defined order in Java (diffcheck
/// compares them as sets).
fn collection_sql(entity: &Entity, p: &Property) -> Result<(String, CollectionKind), NotPorted> {
    let field = p.field_name.as_deref().ok_or(NotPorted("no field"))?;
    let m = entity
        .properties
        .get(field)
        .ok_or(NotPorted("collection not mapped"))?;
    let owner_id = &entity.id.as_ref().ok_or(NotPorted("id"))?.column;
    if m.kind == "element-collection" || (m.element_class.is_none() && m.element_column.is_some()) {
        let table = m.table.as_deref().ok_or(NotPorted("element table"))?;
        let key = m.key_column.as_deref().ok_or(NotPorted("key"))?;
        let elem = m.element_column.as_deref().ok_or(NotPorted("element"))?;
        let order = m
            .index_column
            .as_deref()
            .map_or(String::new(), |i| format!(" order by j.{i}"));
        let string_set = m.kind == "set";
        return Ok((
            format!(
                "(select json_agg(j.{elem}{order}) from {table} j where j.{key} = e.{owner_id})::text"
            ),
            CollectionKind::Values { string_set },
        ));
    }
    if !matches!(m.kind.as_str(), "set" | "list" | "bag") {
        return Err(NotPorted("collection kind"));
    }
    let item_klass = m
        .element_class
        .as_deref()
        .ok_or(NotPorted("element class"))?;
    let item = hibernate::entity(item_klass)
        .filter(|e| e.is_plain_class())
        .ok_or(NotPorted("item mapping"))?;
    let item_schema = registered::by_klass(item_klass).ok_or(NotPorted("item schema"))?;
    if !item_schema.identifiable_object {
        return Err(NotPorted("non-identifiable items"));
    }
    let itable = item.table.as_deref().ok_or(NotPorted("item table"))?;
    let iid = &item.id.as_ref().ok_or(NotPorted("item id"))?.column;
    let users = item_klass == "org.hisp.dhis.user.User" && p.property_transformer;
    let value = if users {
        let c = |f: &str| mapped_column(item, f).ok_or(NotPorted("user column"));
        format!(
            "json_build_array(i.{}, i.{}, i.{}, i.{}, i.{}, i.{})",
            c("uid")?,
            c("code")?,
            c("name")?,
            c("firstName")?,
            c("surname")?,
            c("username")?
        )
    } else {
        format!(
            "i.{}",
            mapped_column(item, "uid").ok_or(NotPorted("item uid"))?
        )
    };
    let kind = if users {
        CollectionKind::Users
    } else {
        CollectionKind::Refs
    };
    // the join rows: the collection's own table, the owning side of a JPA mappedBy, or the
    // item table (one-to-many)
    let (table, key, elem): (String, String, Option<String>) =
        match (&m.table, &m.key_column, &m.element_column, &m.mapped_by) {
            (Some(t), Some(k), Some(el), _) => (t.clone(), k.clone(), Some(el.clone())),
            (None, _, _, Some(mb)) => {
                let o = item.properties.get(mb).ok_or(NotPorted("mappedBy"))?;
                match (o.kind.as_str(), &o.table, &o.key_column, &o.element_column) {
                    ("set" | "list" | "bag", Some(t), Some(k), Some(el)) => {
                        (t.clone(), el.clone(), Some(k.clone()))
                    }
                    ("many-to-one", _, _, _) if o.column_table.is_none() => (
                        itable.to_string(),
                        o.column.clone().ok_or(NotPorted("fk"))?,
                        None,
                    ),
                    // the join column kept in a secondary table of the item, keyed by the item
                    // (e.g. CategoryOptionCombo#categoryCombo in categorycombos_optioncombos)
                    ("many-to-one", _, _, _) => {
                        let t = o.column_table.clone().ok_or(NotPorted("fk table"))?;
                        let pk = match item.secondary_tables.get(&t) {
                            Some(Some(k)) => k.clone(),
                            Some(None) => iid.to_string(),
                            None => return Err(NotPorted("secondary table")),
                        };
                        (t, o.column.clone().ok_or(NotPorted("fk"))?, Some(pk))
                    }
                    _ => return Err(NotPorted("mappedBy shape")),
                }
            }
            (None, Some(k), None, None) => (itable.to_string(), k.clone(), None),
            _ => return Err(NotPorted("collection shape")),
        };
    let order = match (&m.index_column, &m.order_by) {
        (Some(i), _) if elem.is_some() => format!(" order by j.{i}"),
        (Some(i), _) => format!(" order by i.{i}"),
        (None, Some(ob)) => {
            let (prop, dir) = ob.split_once(' ').unwrap_or((ob.as_str(), "ASC"));
            let col = mapped_column(item, prop).ok_or(NotPorted("order by"))?;
            let dir = if dir.eq_ignore_ascii_case("desc") {
                "desc"
            } else {
                "asc"
            };
            format!(" order by i.{col} {dir}")
        }
        (None, None) => String::new(),
    };
    let sql = match elem {
        Some(el) => format!(
            "(select json_agg({value}{order}) from {itable} i join {table} j on i.{iid} = j.{el} where j.{key} = e.{owner_id})::text"
        ),
        None => format!(
            "(select json_agg({value}{order}) from {itable} i where i.{key} = e.{owner_id})::text"
        ),
    };
    Ok((sql, kind))
}

/// The row count of a collection whose items are not rendered here (join table, one-to-many
/// through `mappedBy` or a plain key column on the item table).
fn collection_count_sql(entity: &Entity, p: &Property) -> Option<String> {
    let m = entity.properties.get(p.field_name.as_deref()?)?;
    let owner_id = &entity.id.as_ref()?.column;
    if let (Some(t), Some(k)) = (&m.table, &m.key_column) {
        return Some(format!(
            "(select count(*) from {t} j where j.{k} = e.{owner_id})"
        ));
    }
    let item = hibernate::entity(m.element_class.as_deref()?)?;
    let itable = item.table.as_deref()?;
    let fk = match (&m.key_column, &m.mapped_by) {
        (Some(k), None) => k.clone(),
        (None, Some(mb)) => {
            let o = item.properties.get(mb)?;
            if o.kind != "many-to-one" {
                return None;
            }
            // a join column kept in a secondary table: count there
            if let Some(t) = &o.column_table {
                let c = o.column.as_deref()?;
                return Some(format!(
                    "(select count(*) from {t} c where c.{c} = e.{owner_id})"
                ));
            }
            o.column.clone()?
        }
        _ => return None,
    };
    Some(format!(
        "(select count(*) from {itable} c where c.{fk} = e.{owner_id})"
    ))
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
                    Base::Null => None,
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
            Source::Fixed(n) => n.clone(),
            Source::EmptyOnly(i, why) => match &cells[*i] {
                Cell::Int(0) => Node::Array(vec![]),
                _ => return Err(NotPorted(why)),
            },
            Source::Unanimous(i) => match text(cells, *i) {
                None => continue,
                Some("?") => return Err(NotPorted("members disagree")),
                Some(v) => Node::Scalar(Value::String(v.into())),
            },
            Source::NullOnly(i) => match &cells[*i] {
                Cell::Null => continue,
                _ => return Err(NotPorted("JSON value not reproduced")),
            },
            Source::Uid(i) => s(text(cells, *i)),
            Source::Bool(i) => match &cells[*i] {
                Cell::Bool(b) => Node::Scalar(Value::Bool(*b)),
                _ => return Err(bad),
            },
            Source::IsDefault(i) => Node::Scalar(Value::Bool(text(cells, *i) == Some("default"))),
            Source::FirstRef(i) => {
                let items: Vec<Value> = match text(cells, *i) {
                    None => continue,
                    Some(t) => serde_json::from_str(t).map_err(|_| bad)?,
                };
                match items.first().and_then(|v| v.as_str()) {
                    None => continue,
                    Some(uid) => Node::Bean {
                        klass: "",
                        props: vec![("id".into(), Node::Scalar(Value::String(uid.into())))],
                    },
                }
            }
            Source::Collection { index, kind } => {
                let items: Vec<Value> = match text(cells, *index) {
                    None => Vec::new(),
                    Some(t) => serde_json::from_str(t).map_err(|_| bad)?,
                };
                match kind {
                    CollectionKind::Refs => Node::Array(
                        items
                            .iter()
                            .map(|v| Node::Bean {
                                klass: "",
                                props: vec![("id".into(), Node::Scalar(v.clone()))],
                            })
                            .collect(),
                    ),
                    CollectionKind::IdMaps => Node::Array(
                        items
                            .iter()
                            .map(|v| Node::Map(vec![("id".into(), Node::Scalar(v.clone()))]))
                            .collect(),
                    ),
                    CollectionKind::Users => {
                        let mut out = Vec::new();
                        for v in &items {
                            let a = v.as_array().ok_or(bad)?;
                            let f = |i: usize| a.get(i).and_then(|x| x.as_str());
                            let name = match f(2) {
                                Some(n) => n.to_string(),
                                None => {
                                    format!("{} {}", f(3).unwrap_or("null"), f(4).unwrap_or("null"))
                                }
                            };
                            out.push(Node::Map(vec![
                                ("id".into(), s(f(0))),
                                ("code".into(), s(f(1))),
                                ("name".into(), s(Some(&name))),
                                ("displayName".into(), s(Some(&name))),
                                ("username".into(), s(f(5))),
                            ]));
                        }
                        Node::Array(out)
                    }
                    CollectionKind::Values { string_set } => {
                        if *string_set {
                            let strs: Vec<&str> = items.iter().filter_map(|v| v.as_str()).collect();
                            if strs.len() != items.len() {
                                return Err(bad);
                            }
                            let order = dhis_core_model::java::hash::hash_set_order(
                                strs,
                                dhis_core_model::java::hash::Construction::Default,
                            )
                            .ok_or(bad)?;
                            Node::Array(
                                order
                                    .into_iter()
                                    .map(|s| Node::Scalar(Value::String(s.into())))
                                    .collect(),
                            )
                        } else {
                            Node::Array(items.into_iter().map(Node::Scalar).collect())
                        }
                    }
                }
            }
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
    let parts = RequestParts::of(&req);
    drop(req);
    match object(&state, &user, &uid, &parts, schema).await {
        Ok(r) => r,
        Err(NotPorted(reason)) => fallback(reason),
    }
}

/// What the handlers need of the request (owned, so the future stays `Send`).
pub(crate) struct RequestParts {
    query: Option<String>,
    headers: http::HeaderMap,
    original_path: String,
}

impl RequestParts {
    pub(crate) fn of(req: &Request) -> Self {
        RequestParts {
            query: req.uri().query().map(str::to_string),
            headers: req.headers().clone(),
            original_path: req
                .extensions()
                .get::<RequestContext>()
                .map_or_else(|| req.uri().path().to_string(), |c| c.original_path.clone()),
        }
    }
}

/// What reading objects of a type with a field selection needs, for one user: the ACL user, the
/// translation locale and the row's SQL (columns and joins of table `e`) with the sources of the
/// written properties.
struct Prepared<'u> {
    acl_user: dhis_acl::User<'u>,
    locale: Locale,
    includes: std::collections::BTreeSet<String>,
    table: &'static str,
    uid_col: String,
    sel: Select,
    sources: Vec<(&'static str, Source)>,
    sharing_col: Option<usize>,
    translations_col: Option<usize>,
    uid_index: usize,
}

fn prepare<'u>(
    user: &'u CurrentUser,
    schema: &'static RegisteredSchema,
    fields: &str,
) -> Result<Prepared<'u>, NotPorted> {
    static EMPTY: Vec<String> = Vec::new();
    let authorities = user
        .authorities
        .as_ref()
        .ok_or(NotPorted("no authorities"))?;
    let superuser = user.is_super() == Some(true);
    let identity = user.identity.as_ref();
    if !superuser && identity.is_none() {
        return Err(NotPorted("session identity unknown"));
    }
    let acl_user = dhis_acl::User {
        superuser,
        uid: identity.map_or("", |i| i.uid.as_str()),
        group_ids: identity.map_or(&EMPTY, |i| &i.group_ids),
        authorities,
    };
    if !dhis_acl::can_read_type(&acl_user, schema) {
        return Err(NotPorted("type not readable"));
    }
    let locale_text = translation_locale(user, "").ok_or(NotPorted("locale"))?;
    let locale = Locale::parse(&locale_text).map_err(|_| NotPorted("locale"))?;

    let paths = dhis_query::fields::parse(fields).map_err(|_| NotPorted("fields"))?;
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
    Ok(Prepared {
        acl_user,
        locale,
        includes,
        table,
        uid_col,
        sel,
        sources,
        sharing_col,
        translations_col,
        uid_index,
    })
}

impl Prepared<'_> {
    fn select_list(&self) -> String {
        let list: Vec<&str> = self.sel.columns.iter().map(|(e, _)| e.as_str()).collect();
        list.join(", ")
    }

    fn cells(&self, row: &sqlx::postgres::PgRow) -> Result<Vec<Cell>, NotPorted> {
        let mut cells = Vec::with_capacity(self.sel.columns.len());
        for (i, (_, ty)) in self.sel.columns.iter().enumerate() {
            cells.push(decode(row, i, *ty).ok_or(NotPorted("column type"))?);
        }
        Ok(cells)
    }

    /// `applySharingDisplayNames` (only when `sharing` is written): the display names of the
    /// users and groups in the row's sharing, added to `names`.
    async fn sharing_display_names(
        &self,
        db: &sqlx::PgPool,
        cells: &[Cell],
        names: &mut std::collections::HashMap<String, Option<String>>,
    ) -> Result<(), NotPorted> {
        if !self.includes.contains("sharing") {
            return Ok(());
        }
        let Some(text) = self.sharing_col.and_then(|i| self::text(cells, i)) else {
            return Ok(());
        };
        let sharing = acl_sharing(Some(text)).ok_or(NotPorted("sharing"))?;
        for (id, _) in &sharing.users {
            if !names.contains_key(id) {
                let n = user_display_name(db, id).await?;
                names.insert(id.clone(), n);
            }
        }
        for (id, _) in &sharing.user_groups {
            if !names.contains_key(id) {
                let n = group_display_name(db, id, &self.locale).await?;
                names.insert(id.clone(), n);
            }
        }
        Ok(())
    }

    /// The row as the field filter writes it.
    fn row_json(
        &self,
        schema: &'static RegisteredSchema,
        cells: &[Cell],
        api_path: &str,
        names: &std::collections::HashMap<String, Option<String>>,
    ) -> Result<String, NotPorted> {
        let href = format!(
            "{api_path}{}/{}",
            schema.relative_api_endpoint.as_deref().unwrap_or(""),
            text(cells, self.uid_index).unwrap_or("")
        );
        let ctx = RenderCtx {
            schema,
            locale: self.locale.clone(),
            href,
            acl_user: self.acl_user.clone(),
            display_names: names,
        };
        let node = render(
            &self.sources,
            cells,
            &ctx,
            self.sharing_col,
            self.translations_col,
        )?;
        Ok(filter(&node, &self.includes).to_json_string())
    }
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
    // getFieldsObject(): `*` unless given
    let fields = {
        let all = params.all("fields");
        if all.is_empty() || all.iter().all(|f| f.is_empty()) {
            "*".to_string()
        } else {
            all.join(",")
        }
    };
    let prep = prepare(user, schema, &fields)?;
    let entity = hibernate::entity(&schema.klass).ok_or(NotPorted("not mapped"))?;

    // manager.get: the object, if the user can read it (sharing)
    let access_sql =
        if prep.acl_user.superuser || schema.property("sharing").is_none_or(|p| !p.persisted) {
            "1=1".to_string()
        } else {
            dhis_query::gist::plan::sharing_condition(
                "e",
                entity,
                prep.acl_user.uid,
                prep.acl_user.group_ids,
            )
            .ok_or(NotPorted("sharing filter"))?
        };
    let sql = format!(
        "select {} from {} e{} where e.{} = $1 and ({access_sql})",
        prep.select_list(),
        prep.table,
        prep.sel.joins.concat(),
        prep.uid_col
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
    let cells = prep.cells(&row)?;
    let mut names = std::collections::HashMap::new();
    prep.sharing_display_names(db, &cells, &mut names).await?;
    let json = prep.row_json(
        schema,
        &cells,
        &api_path(&req.headers, &req.original_path),
        &names,
    )?;
    let mut r = json_response(json);
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
