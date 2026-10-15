//! Evaluates constant expressions (annotation values, `static final` initialisers) against the
//! Java index, following Java's name resolution closely enough for annotation attributes.

use crate::java::{Expr, JavaIndex, TypeDecl};

/// Constants from libraries outside dhis2-core that controllers use in mapping annotations.
/// Values are the published constants of the named library classes.
const EXTERNAL: &[(&str, &str)] = &[
    ("MediaType.ALL_VALUE", "*/*"),
    (
        "MediaType.APPLICATION_ATOM_XML_VALUE",
        "application/atom+xml",
    ),
    ("MediaType.APPLICATION_CBOR_VALUE", "application/cbor"),
    (
        "MediaType.APPLICATION_FORM_URLENCODED_VALUE",
        "application/x-www-form-urlencoded",
    ),
    (
        "MediaType.APPLICATION_GRAPHQL_RESPONSE_VALUE",
        "application/graphql-response+json",
    ),
    ("MediaType.APPLICATION_JSON_VALUE", "application/json"),
    (
        "MediaType.APPLICATION_JSON_UTF8_VALUE",
        "application/json;charset=UTF-8",
    ),
    ("MediaType.APPLICATION_NDJSON_VALUE", "application/x-ndjson"),
    (
        "MediaType.APPLICATION_OCTET_STREAM_VALUE",
        "application/octet-stream",
    ),
    ("MediaType.APPLICATION_PDF_VALUE", "application/pdf"),
    (
        "MediaType.APPLICATION_PROBLEM_JSON_VALUE",
        "application/problem+json",
    ),
    (
        "MediaType.APPLICATION_PROBLEM_XML_VALUE",
        "application/problem+xml",
    ),
    (
        "MediaType.APPLICATION_PROTOBUF_VALUE",
        "application/x-protobuf",
    ),
    ("MediaType.APPLICATION_RSS_XML_VALUE", "application/rss+xml"),
    (
        "MediaType.APPLICATION_XHTML_XML_VALUE",
        "application/xhtml+xml",
    ),
    ("MediaType.APPLICATION_XML_VALUE", "application/xml"),
    ("MediaType.APPLICATION_YAML_VALUE", "application/yaml"),
    ("MediaType.IMAGE_GIF_VALUE", "image/gif"),
    ("MediaType.IMAGE_JPEG_VALUE", "image/jpeg"),
    ("MediaType.IMAGE_PNG_VALUE", "image/png"),
    ("MediaType.MULTIPART_FORM_DATA_VALUE", "multipart/form-data"),
    ("MediaType.MULTIPART_MIXED_VALUE", "multipart/mixed"),
    ("MediaType.MULTIPART_RELATED_VALUE", "multipart/related"),
    ("MediaType.TEXT_EVENT_STREAM_VALUE", "text/event-stream"),
    ("MediaType.TEXT_HTML_VALUE", "text/html"),
    ("MediaType.TEXT_MARKDOWN_VALUE", "text/markdown"),
    ("MediaType.TEXT_PLAIN_VALUE", "text/plain"),
    ("MediaType.TEXT_XML_VALUE", "text/xml"),
    // org.springframework.util.MimeTypeUtils (same values as MediaType).
    ("MimeTypeUtils.APPLICATION_JSON_VALUE", "application/json"),
    ("MimeTypeUtils.TEXT_PLAIN_VALUE", "text/plain"),
    // io.prometheus.client.exporter.common.TextFormat (simpleclient_common 0.16.0).
    (
        "TextFormat.CONTENT_TYPE_004",
        "text/plain; version=0.0.4; charset=utf-8",
    ),
];

pub struct Eval<'a> {
    pub idx: &'a JavaIndex,
}

/// The evaluation failed; carries the source-ish text of what could not be resolved.
#[derive(Debug, Clone, PartialEq)]
pub struct Unresolved(pub String);

impl<'a> Eval<'a> {
    pub fn new(idx: &'a JavaIndex) -> Self {
        Self { idx }
    }

    /// Evaluates to a list of strings (arrays flatten, scalars become one element).
    pub fn strings(&self, e: &Expr, ctx: &TypeDecl) -> Result<Vec<String>, Unresolved> {
        match e {
            Expr::Array(items) => {
                let mut out = Vec::new();
                for i in items {
                    out.extend(self.strings(i, ctx)?);
                }
                Ok(out)
            }
            other => Ok(vec![self.string(other, ctx, 0)?]),
        }
    }

    /// Like [`Self::strings`] but enum constants and other names that are not string constants
    /// evaluate to their simple name (for `RequestMethod.GET`, `Authorities.F_X`, ...).
    pub fn names(&self, e: &Expr, ctx: &TypeDecl) -> Vec<String> {
        match e {
            Expr::Array(items) => items.iter().flat_map(|i| self.names(i, ctx)).collect(),
            Expr::Name(parts) => match self.string(e, ctx, 0) {
                Ok(s) => vec![s],
                Err(_) => vec![parts.last().cloned().unwrap_or_default()],
            },
            other => vec![
                self.string(other, ctx, 0)
                    .unwrap_or_else(|u| format!("?{}", u.0)),
            ],
        }
    }

    pub fn string(&self, e: &Expr, ctx: &TypeDecl, depth: usize) -> Result<String, Unresolved> {
        if depth > 32 {
            return Err(Unresolved("<recursion>".into()));
        }
        match e {
            Expr::Str(s) => Ok(s.clone()),
            Expr::Int(i) => Ok(i.to_string()),
            Expr::Bool(b) => Ok(b.to_string()),
            Expr::Concat(parts) => {
                let mut s = String::new();
                for p in parts {
                    s.push_str(&self.string(p, ctx, depth + 1)?);
                }
                Ok(s)
            }
            Expr::Name(parts) => self.name(parts, ctx, depth),
            Expr::Call { target, name, args } => {
                self.call(target.as_deref(), name, args, ctx, depth)
            }
            other => Err(Unresolved(format!("{other:?}"))),
        }
    }

    fn call(
        &self,
        target: Option<&Expr>,
        name: &str,
        args: &[Expr],
        ctx: &TypeDecl,
        depth: usize,
    ) -> Result<String, Unresolved> {
        let target_name = match target {
            Some(Expr::Name(p)) => p.join("."),
            _ => String::new(),
        };
        match (target_name.as_str(), name, args) {
            ("String", "valueOf", [a]) | ("Integer" | "Long", "toString", [a]) => {
                self.string(a, ctx, depth + 1)
            }
            (unit, conv, [a]) if conv.starts_with("to") => {
                let n: i64 = self
                    .string(a, ctx, depth + 1)?
                    .parse()
                    .map_err(|_| Unresolved(format!("{unit}.{conv}")))?;
                let unit = unit.rsplit('.').next().unwrap_or(unit);
                let in_millis = match unit {
                    "MILLISECONDS" => 1,
                    "SECONDS" => 1_000,
                    "MINUTES" => 60_000,
                    "HOURS" => 3_600_000,
                    "DAYS" => 86_400_000,
                    _ => return Err(Unresolved(format!("{unit}.{conv}(..)"))),
                };
                let out = match conv {
                    "toMillis" => n * in_millis,
                    "toSeconds" => n * in_millis / 1_000,
                    "toMinutes" => n * in_millis / 60_000,
                    _ => return Err(Unresolved(format!("{unit}.{conv}(..)"))),
                };
                Ok(out.to_string())
            }
            _ => Err(Unresolved(format!("{target_name}.{name}(..)"))),
        }
    }

    fn name(&self, parts: &[String], ctx: &TypeDecl, depth: usize) -> Result<String, Unresolved> {
        let joined = parts.join(".");
        if parts.len() == 1 {
            let field = &parts[0];
            if let Some((e, owner)) = self.find_field(ctx, field) {
                return self.string(e, owner, depth + 1);
            }
            let file = &self.idx.files[ctx.file_idx];
            if let Some(cls) = file.static_imports.get(field) {
                return self.qualified(cls, field, ctx, depth);
            }
            for cls in &file.static_star_imports {
                if let Ok(v) = self.qualified(cls, field, ctx, depth) {
                    return Ok(v);
                }
            }
            return Err(Unresolved(joined));
        }
        let (cls, field) = parts.split_at(parts.len() - 1);
        self.qualified(&cls.join("."), &field[0], ctx, depth)
    }

    fn qualified(
        &self,
        cls: &str,
        field: &str,
        ctx: &TypeDecl,
        depth: usize,
    ) -> Result<String, Unresolved> {
        if let Some(t) = self
            .idx
            .resolve_type(ctx, cls)
            .or_else(|| self.idx.by_fqn(cls))
            && let Some((e, owner)) = self.find_field(t, field)
        {
            return self.string(e, owner, depth + 1);
        }
        let simple = cls.rsplit('.').next().unwrap_or(cls);
        let key = format!("{simple}.{field}");
        EXTERNAL
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.to_string())
            .ok_or(Unresolved(format!("{cls}.{field}")))
    }

    /// Finds a constant in `t`, its outer types and its supertypes.
    fn find_field<'b>(&'b self, t: &'b TypeDecl, field: &str) -> Option<(&'b Expr, &'b TypeDecl)> {
        let mut seen = Vec::new();
        self.find_field_rec(t, field, &mut seen)
    }

    fn find_field_rec<'b>(
        &'b self,
        t: &'b TypeDecl,
        field: &str,
        seen: &mut Vec<String>,
    ) -> Option<(&'b Expr, &'b TypeDecl)> {
        if seen.contains(&t.fqn) {
            return None;
        }
        seen.push(t.fqn.clone());
        if let Some(e) = t.constants.get(field) {
            return Some((e, t));
        }
        let supers = t.superclass.iter().chain(t.interfaces.iter());
        for s in supers {
            if let Some(st) = self.idx.resolve_type(t, s)
                && let Some(found) = self.find_field_rec(st, field, seen)
            {
                return Some(found);
            }
        }
        // Outer type.
        if let Some((outer, _)) = t.fqn.rsplit_once('.')
            && let Some(ot) = self.idx.by_fqn(outer)
        {
            return self.find_field_rec(ot, field, seen);
        }
        None
    }
}
