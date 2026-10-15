//! Spring MVC mappings of `dhis-web-api` -> `docs/inventory/endpoints.csv`.
//!
//! Mirrors how Spring + DHIS2's `CustomRequestMappingHandlerMapping` register handlers:
//! - handler types are concrete classes annotated `@Controller`/`@RestController`;
//! - mapped methods are collected from the class and all supertypes; an override without a
//!   mapping annotation inherits the super method's mapping, an override with one replaces it;
//! - the type-level `@RequestMapping` is the nearest one in the type hierarchy;
//! - type and method patterns are combined like `PathPattern#combine`;
//! - a mapping without HTTP methods defaults to GET (DHIS2-specific).

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use crate::eval::Eval;
use crate::java::{Annotation, Expr, JavaIndex, Method, TypeDecl, TypeKind};

pub const COLUMNS: &[&str] = &[
    "status",
    "http_method",
    "path",
    "controller",
    "handler",
    "declared_in",
    "source",
    "params_condition",
    "headers_condition",
    "consumes",
    "produces",
    "path_variables",
    "request_params",
    "request_headers",
    "request_body",
    "bound_params",
    "authorities",
    "deprecated",
];

#[derive(Debug, Clone)]
pub struct Endpoint {
    pub http_method: String,
    pub path: String,
    pub controller: String,
    pub handler: String,
    pub declared_in: String,
    pub source: String,
    pub params_condition: String,
    pub headers_condition: String,
    pub consumes: String,
    pub produces: String,
    pub path_variables: String,
    pub request_params: String,
    pub request_headers: String,
    pub request_body: String,
    pub bound_params: String,
    pub authorities: String,
    pub deprecated: bool,
}

impl Endpoint {
    /// Stable identity used to carry the `status` column across regenerations.
    pub fn key(&self) -> String {
        format!(
            "{} {} {}#{} [{}|{}|{}|{}]",
            self.http_method,
            self.path,
            self.controller,
            self.handler,
            self.params_condition,
            self.headers_condition,
            self.consumes,
            self.produces
        )
    }

    pub fn record(&self, status: &str) -> Vec<String> {
        vec![
            status.to_string(),
            self.http_method.clone(),
            self.path.clone(),
            self.controller.clone(),
            self.handler.clone(),
            self.declared_in.clone(),
            self.source.clone(),
            self.params_condition.clone(),
            self.headers_condition.clone(),
            self.consumes.clone(),
            self.produces.clone(),
            self.path_variables.clone(),
            self.request_params.clone(),
            self.request_headers.clone(),
            self.request_body.clone(),
            self.bound_params.clone(),
            self.authorities.clone(),
            self.deprecated.to_string(),
        ]
    }

    pub fn key_from_record(r: &csv::StringRecord) -> String {
        let g = |i: usize| r.get(i).unwrap_or("");
        format!(
            "{} {} {}#{} [{}|{}|{}|{}]",
            g(1),
            g(2),
            g(3),
            g(4),
            g(7),
            g(8),
            g(9),
            g(10)
        )
    }
}

const MAPPING_ANNOTATIONS: &[(&str, Option<&str>)] = &[
    ("RequestMapping", None),
    ("GetMapping", Some("GET")),
    ("PostMapping", Some("POST")),
    ("PutMapping", Some("PUT")),
    ("DeleteMapping", Some("DELETE")),
    ("PatchMapping", Some("PATCH")),
];

fn mapping_of(anns: &[Annotation]) -> Option<(&Annotation, Option<&'static str>)> {
    anns.iter().find_map(|a| {
        MAPPING_ANNOTATIONS
            .iter()
            .find(|(n, _)| *n == a.name)
            .map(|(_, m)| (a, *m))
    })
}

/// Infrastructure argument types Spring or DHIS2 resolve without reading request params.
const INFRA_TYPES: &[&str] = &[
    "HttpServletRequest",
    "HttpServletResponse",
    "ServletRequest",
    "ServletResponse",
    "HttpSession",
    "WebRequest",
    "NativeWebRequest",
    "Model",
    "ModelMap",
    "Principal",
    "Authentication",
    "Locale",
    "TimeZone",
    "ZoneId",
    "InputStream",
    "OutputStream",
    "Reader",
    "Writer",
    "BindingResult",
    "Errors",
    "UriComponentsBuilder",
    "SessionStatus",
    "HttpMethod",
    "SystemSettings",
    "MultipartRequest",
    "MultipartHttpServletRequest",
];

/// `BeanUtils.isSimpleProperty`-like types: bound as an implicit optional request param.
const SIMPLE_TYPES: &[&str] = &[
    "String",
    "int",
    "Integer",
    "long",
    "Long",
    "boolean",
    "Boolean",
    "double",
    "Double",
    "float",
    "Float",
    "short",
    "Short",
    "byte",
    "Byte",
    "char",
    "Character",
    "Date",
    "Instant",
    "LocalDate",
    "LocalDateTime",
    "ZonedDateTime",
    "BigDecimal",
    "BigInteger",
    "URI",
    "URL",
    "Class",
    "CharSequence",
    "Number",
];

pub struct Report {
    pub endpoints: Vec<Endpoint>,
    pub warnings: Vec<String>,
}

pub fn extract(idx: &JavaIndex, java_root: &Path) -> Report {
    let ev = Eval::new(idx);
    let mut endpoints = Vec::new();
    let mut warnings = Vec::new();

    for t in &idx.types {
        let is_controller = t
            .annotations
            .iter()
            .any(|a| a.name == "Controller" || a.name == "RestController");
        if !is_controller || t.is_abstract || t.kind != TypeKind::Class {
            continue;
        }
        let hierarchy = hierarchy(idx, t);

        // Type-level mapping: nearest in the hierarchy.
        let type_mapping = hierarchy
            .iter()
            .find_map(|h| mapping_of(&h.annotations).map(|(a, _)| (a, *h)));
        let type_paths = match type_mapping {
            Some((a, owner)) => paths_of(&ev, a, owner, &mut warnings),
            None => vec![],
        };
        let type_auth: Vec<String> = hierarchy
            .iter()
            .flat_map(|h| authority_annotations(&ev, &h.annotations, h))
            .collect();
        let type_deprecated = t.annotations.iter().any(|a| a.name == "Deprecated");

        // Methods: most specific mapping wins per (name, arity).
        let mut seen: HashSet<(String, usize)> = HashSet::new();
        for owner in &hierarchy {
            for m in &owner.methods {
                let Some((ann, fixed_method)) = mapping_of(&m.annotations) else {
                    continue;
                };
                if !seen.insert((m.name.clone(), m.params.len())) {
                    continue;
                }
                let method_paths = paths_of(&ev, ann, owner, &mut warnings);
                let http_methods: Vec<String> = match fixed_method {
                    Some(fm) => vec![fm.to_string()],
                    None => {
                        let ms: Vec<String> = ann
                            .arg("method")
                            .map(|e| ev.names(e, owner))
                            .unwrap_or_default();
                        if ms.is_empty() {
                            vec!["GET".into()]
                        } else {
                            ms
                        }
                    }
                };
                let mut attr = |name: &str| -> String {
                    ann.arg(name)
                        .map(|e| match ev.strings(e, owner) {
                            Ok(v) => v.join(";"),
                            Err(u) => {
                                warnings.push(format!(
                                    "{}#{}: unresolved {name}: {}",
                                    owner.simple_name, m.name, u.0
                                ));
                                format!("?{}", u.0)
                            }
                        })
                        .unwrap_or_default()
                };
                let params_condition = attr("params");
                let headers_condition = attr("headers");
                let consumes = attr("consumes");
                let produces = attr("produces");
                let args = describe_params(&ev, idx, owner, m);
                let mut auth = type_auth.clone();
                auth.extend(authority_annotations(&ev, &m.annotations, owner));
                let deprecated =
                    type_deprecated || m.annotations.iter().any(|a| a.name == "Deprecated");
                let source = format!(
                    "{}:{}",
                    owner
                        .file
                        .strip_prefix(java_root)
                        .unwrap_or(&owner.file)
                        .display(),
                    m.line
                );

                for full in combine_all(&type_paths, &method_paths) {
                    for hm in &http_methods {
                        endpoints.push(Endpoint {
                            http_method: hm.clone(),
                            path: full.clone(),
                            controller: t.simple_name.clone(),
                            handler: m.name.clone(),
                            declared_in: if owner.fqn == t.fqn {
                                String::new()
                            } else {
                                owner.simple_name.clone()
                            },
                            source: source.clone(),
                            params_condition: params_condition.clone(),
                            headers_condition: headers_condition.clone(),
                            consumes: consumes.clone(),
                            produces: produces.clone(),
                            path_variables: args.path_variables.join(";"),
                            request_params: args.request_params.join(";"),
                            request_headers: args.request_headers.join(";"),
                            request_body: args.request_body.join(";"),
                            bound_params: args.bound.join(";"),
                            authorities: auth.join(" & "),
                            deprecated,
                        });
                    }
                }
            }
        }
    }

    endpoints.sort_by(|a, b| {
        (
            &a.path,
            &a.http_method,
            &a.controller,
            &a.handler,
            &a.params_condition,
        )
            .cmp(&(
                &b.path,
                &b.http_method,
                &b.controller,
                &b.handler,
                &b.params_condition,
            ))
    });
    warnings.sort();
    warnings.dedup();
    Report {
        endpoints,
        warnings,
    }
}

/// The type itself, then superclasses, then all interfaces (breadth-first), de-duplicated.
fn hierarchy<'a>(idx: &'a JavaIndex, t: &'a TypeDecl) -> Vec<&'a TypeDecl> {
    let mut out: Vec<&TypeDecl> = Vec::new();
    let mut classes = vec![t];
    let mut cur = t;
    while let Some(sup) = cur
        .superclass
        .as_deref()
        .and_then(|s| idx.resolve_type(cur, s))
    {
        if classes.iter().any(|c| c.fqn == sup.fqn) {
            break;
        }
        classes.push(sup);
        cur = sup;
    }
    out.extend(classes.iter().copied());
    let mut queue: Vec<&TypeDecl> = classes;
    let mut i = 0;
    while i < queue.len() {
        let c = queue[i];
        for iface in &c.interfaces {
            if let Some(it) = idx.resolve_type(c, iface)
                && !out.iter().any(|o| o.fqn == it.fqn)
            {
                out.push(it);
                queue.push(it);
            }
        }
        i += 1;
    }
    out
}

fn paths_of(
    ev: &Eval,
    a: &Annotation,
    owner: &TypeDecl,
    warnings: &mut Vec<String>,
) -> Vec<String> {
    let e = a.arg("value").or_else(|| a.arg("path"));
    match e {
        None => vec![],
        Some(e) => match ev.strings(e, owner) {
            Ok(v) => v,
            Err(u) => {
                warnings.push(format!("{}: unresolved path: {}", owner.simple_name, u.0));
                vec![format!("?{}", u.0)]
            }
        },
    }
}

/// `PathPatternsRequestCondition` prepends `/` to non-empty patterns lacking it.
fn normalize_pattern(p: &str) -> String {
    if !p.is_empty() && !p.starts_with('/') {
        format!("/{p}")
    } else {
        p.to_string()
    }
}

fn combine_all(type_paths: &[String], method_paths: &[String]) -> Vec<String> {
    let tps: Vec<String> = type_paths.iter().map(|p| normalize_pattern(p)).collect();
    let mps: Vec<String> = method_paths.iter().map(|p| normalize_pattern(p)).collect();
    match (tps.is_empty(), mps.is_empty()) {
        (true, true) => vec![String::new()],
        (true, false) => mps,
        (false, true) => tps,
        (false, false) => {
            let mut out = Vec::new();
            for t in &tps {
                for m in &mps {
                    out.push(combine(t, m));
                }
            }
            out
        }
    }
}

/// `PathPattern#combine` for the shapes that occur in DHIS2 (no `*.ext` type patterns).
pub fn combine(p1: &str, p2: &str) -> String {
    if p1.is_empty() {
        return p2.to_string();
    }
    if p2.is_empty() {
        return p1.to_string();
    }
    if let Some(prefix) = p1.strip_suffix("/*").or_else(|| p1.strip_suffix("/**")) {
        return concat(prefix, p2);
    }
    concat(p1, p2)
}

fn concat(p1: &str, p2: &str) -> String {
    match (p1.ends_with('/'), p2.starts_with('/')) {
        (true, true) => format!("{p1}{}", &p2[1..]),
        (true, false) | (false, true) => format!("{p1}{p2}"),
        (false, false) => format!("{p1}/{p2}"),
    }
}

fn authority_annotations(ev: &Eval, anns: &[Annotation], owner: &TypeDecl) -> Vec<String> {
    let mut out = Vec::new();
    for a in anns {
        match a.name.as_str() {
            "RequiresAuthority" => {
                let any: Vec<String> = a
                    .arg("anyOf")
                    .map(|e| ev.names(e, owner))
                    .unwrap_or_default();
                out.push(format!("anyOf({})", any.join("|")));
            }
            "PreAuthorize" | "Secured" | "RolesAllowed" => {
                let v = a
                    .arg("value")
                    .map(|e| ev.names(e, owner).join("|"))
                    .unwrap_or_default();
                out.push(format!("{}({v})", a.name));
            }
            _ => {}
        }
    }
    out
}

#[derive(Default)]
struct Args {
    path_variables: Vec<String>,
    request_params: Vec<String>,
    request_headers: Vec<String>,
    request_body: Vec<String>,
    bound: Vec<String>,
}

fn describe_params(ev: &Eval, idx: &JavaIndex, owner: &TypeDecl, m: &Method) -> Args {
    let mut a = Args::default();
    for p in &m.params {
        let ann = |n: &str| p.annotations.iter().find(|x| x.name == n);
        let name_of = |x: &Annotation| -> String {
            x.arg("value")
                .or_else(|| x.arg("name"))
                .and_then(|e| ev.string(e, owner, 0).ok())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| p.name.clone())
        };
        let erased = crate::java::erase(&p.ty);
        if let Some(x) = ann("PathVariable") {
            a.path_variables.push(name_of(x));
        } else if let Some(x) = ann("RequestParam").or_else(|| ann("RequestPart")) {
            if erased == "Map" || erased == "MultiValueMap" {
                a.request_params.push("*".into());
                continue;
            }
            let required = x
                .arg("required")
                .map(|e| *e != Expr::Bool(false))
                .unwrap_or(true)
                && x.arg("defaultValue").is_none()
                && erased != "Optional";
            let suffix = if required { "" } else { "?" };
            let part = if x.name == "RequestPart" { "part:" } else { "" };
            a.request_params
                .push(format!("{part}{}{suffix}", name_of(x)));
        } else if let Some(x) = ann("RequestHeader") {
            a.request_headers.push(name_of(x));
        } else if ann("RequestBody").is_some() {
            a.request_body.push(p.ty.clone());
        } else if ann("CurrentUser").is_some()
            || ann("CookieValue").is_some()
            || INFRA_TYPES.contains(&erased.as_str())
        {
            continue;
        } else if SIMPLE_TYPES.contains(&erased.as_str())
            || idx
                .resolve_type(owner, &erased)
                .is_some_and(|t| t.kind == TypeKind::Enum)
        {
            a.request_params.push(format!("{}?", p.name));
        } else {
            a.bound.push(format!("{}:{}", p.name, p.ty));
        }
    }
    a
}

/// Reads `status` per endpoint key from an existing CSV so regeneration keeps progress.
pub fn existing_status(path: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Ok(mut rdr) = csv::Reader::from_path(path) else {
        return out;
    };
    for r in rdr.records().flatten() {
        out.insert(
            Endpoint::key_from_record(&r),
            r.get(0).unwrap_or("java").to_string(),
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combine_like_spring() {
        assert_eq!(combine("/api/dataSets", "/{uid}"), "/api/dataSets/{uid}");
        assert_eq!(combine("/api/dataSets/", "/{uid}"), "/api/dataSets/{uid}");
        assert_eq!(combine("/api/x", ""), "/api/x");
        assert_eq!(combine("/api/x/*", "/y"), "/api/x/y");
        assert_eq!(
            combine_all(&["/api/me".into()], &["dashboard".into()]),
            vec!["/api/me/dashboard"]
        );
    }
}
