//! A small owned model of Java source, extracted with tree-sitter-java.
//!
//! Only what the inventory needs is kept: type declarations, their annotations, methods,
//! `static final` fields and enum constants, plus a tiny expression AST for annotation values and
//! constant initialisers.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tree_sitter::{Node, Parser};

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Str(String),
    Int(i64),
    Bool(bool),
    /// `foo` or `a.b.C.FOO` (dotted names, enum constants, constants).
    Name(Vec<String>),
    Concat(Vec<Expr>),
    Array(Vec<Expr>),
    ClassLit(String),
    Call {
        target: Option<Box<Expr>>,
        name: String,
        args: Vec<Expr>,
    },
    Annotation(Box<Annotation>),
    /// Anything else, kept as source text.
    Other(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Annotation {
    /// Simple name (last segment), e.g. `GetMapping` for `@org.x.GetMapping`.
    pub name: String,
    /// `(element, value)`; a single unnamed value is stored as `value`.
    pub args: Vec<(String, Expr)>,
}

impl Annotation {
    pub fn arg(&self, name: &str) -> Option<&Expr> {
        self.args.iter().find(|(k, _)| k == name).map(|(_, v)| v)
    }
}

#[derive(Debug, Clone)]
pub struct Param {
    pub annotations: Vec<Annotation>,
    pub ty: String,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct Method {
    pub name: String,
    pub annotations: Vec<Annotation>,
    pub params: Vec<Param>,
    pub line: usize,
    /// Source text of the body, for the few places that need to look inside.
    pub body: Option<String>,
    pub returns: Vec<Expr>,
}

#[derive(Debug, Clone)]
pub struct EnumConstant {
    pub name: String,
    pub args: Vec<Expr>,
    pub doc: Option<String>,
    pub annotations: Vec<Annotation>,
}

#[derive(Debug, Clone)]
pub struct Field {
    pub name: String,
    pub ty: String,
    pub annotations: Vec<Annotation>,
    pub is_static: bool,
    pub is_transient: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeKind {
    Class,
    Interface,
    Enum,
    Record,
    Annotation,
}

#[derive(Debug, Clone)]
pub struct TypeDecl {
    /// `pkg.Outer.Inner`.
    pub fqn: String,
    pub simple_name: String,
    pub kind: TypeKind,
    pub is_abstract: bool,
    /// Superclass simple name, generics stripped.
    pub superclass: Option<String>,
    pub interfaces: Vec<String>,
    pub annotations: Vec<Annotation>,
    pub methods: Vec<Method>,
    /// `static final` (or interface) fields with initialisers.
    pub constants: HashMap<String, Expr>,
    /// All field declarations (one entry per declarator).
    pub fields: Vec<Field>,
    pub enum_constants: Vec<EnumConstant>,
    pub file: PathBuf,
    /// Index into `JavaIndex::files` for import resolution.
    pub file_idx: usize,
}

#[derive(Debug, Clone, Default)]
pub struct FileInfo {
    pub package: String,
    /// simple name -> fqn, from single-type imports.
    pub imports: HashMap<String, String>,
    /// `import static a.b.C.NAME;` NAME -> `a.b.C`.
    pub static_imports: HashMap<String, String>,
    /// `import static a.b.C.*;` -> `a.b.C`.
    pub static_star_imports: Vec<String>,
}

#[derive(Debug, Default)]
pub struct JavaIndex {
    pub files: Vec<FileInfo>,
    pub types: Vec<TypeDecl>,
    by_fqn: HashMap<String, usize>,
    by_simple: HashMap<String, Vec<usize>>,
}

impl JavaIndex {
    pub fn add_file(&mut self, path: &Path, src: &str) -> Result<()> {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_java::LANGUAGE.into())
            .context("loading tree-sitter-java")?;
        let tree = parser
            .parse(src, None)
            .with_context(|| format!("parsing {}", path.display()))?;
        let root = tree.root_node();
        let b = src.as_bytes();

        let mut info = FileInfo::default();
        let mut cursor = root.walk();
        for child in root.named_children(&mut cursor) {
            match child.kind() {
                "package_declaration" => {
                    if let Some(n) = first_named(child, &["scoped_identifier", "identifier"]) {
                        info.package = text(n, b).to_string();
                    }
                }
                "import_declaration" => {
                    let t = text(child, b);
                    let is_static = t.contains("import static");
                    let star = t.contains(".*");
                    if let Some(n) = first_named(child, &["scoped_identifier", "identifier"]) {
                        let name = text(n, b).to_string();
                        match (is_static, star) {
                            (true, true) => info.static_star_imports.push(name),
                            (true, false) => {
                                if let Some((cls, member)) = name.rsplit_once('.') {
                                    info.static_imports.insert(member.into(), cls.into());
                                }
                            }
                            (false, false) => {
                                let simple = name.rsplit('.').next().unwrap_or(&name).to_string();
                                info.imports.insert(simple, name);
                            }
                            (false, true) => {}
                        }
                    }
                }
                _ => {}
            }
        }
        let file_idx = self.files.len();
        let package = info.package.clone();
        self.files.push(info);

        let mut cursor = root.walk();
        for child in root.named_children(&mut cursor) {
            self.collect_type(child, b, &package, None, path, file_idx);
        }
        Ok(())
    }

    fn collect_type(
        &mut self,
        node: Node,
        b: &[u8],
        package: &str,
        outer: Option<&str>,
        path: &Path,
        file_idx: usize,
    ) {
        let kind = match node.kind() {
            "class_declaration" => TypeKind::Class,
            "interface_declaration" => TypeKind::Interface,
            "enum_declaration" => TypeKind::Enum,
            "record_declaration" => TypeKind::Record,
            "annotation_type_declaration" => TypeKind::Annotation,
            _ => return,
        };
        let Some(name_node) = node.child_by_field_name("name") else {
            return;
        };
        let simple_name = text(name_node, b).to_string();
        let fqn = match outer {
            Some(o) => format!("{o}.{simple_name}"),
            None if package.is_empty() => simple_name.clone(),
            None => format!("{package}.{simple_name}"),
        };

        let (annotations, modifiers_text) = modifiers(node, b);
        let superclass = node
            .child_by_field_name("superclass")
            .and_then(|s| s.named_child(0))
            .map(|t| erase(text(t, b)));
        let mut interfaces = Vec::new();
        let iface_node = node
            .child_by_field_name("interfaces")
            .or_else(|| find_child(node, "extends_interfaces"));
        if let Some(i) = iface_node
            && let Some(list) = find_child(i, "type_list")
        {
            let mut c = list.walk();
            for t in list.named_children(&mut c) {
                interfaces.push(erase(text(t, b)));
            }
        }

        let mut decl = TypeDecl {
            fqn: fqn.clone(),
            simple_name: simple_name.clone(),
            kind,
            is_abstract: modifiers_text.contains("abstract") || kind == TypeKind::Interface,
            superclass,
            interfaces,
            annotations,
            methods: Vec::new(),
            constants: HashMap::new(),
            fields: Vec::new(),
            enum_constants: Vec::new(),
            file: path.to_path_buf(),
            file_idx,
        };

        let Some(body) = node.child_by_field_name("body") else {
            return;
        };
        let mut nested = Vec::new();
        let mut members: Vec<Node> = Vec::new();
        let mut c = body.walk();
        for m in body.named_children(&mut c) {
            if m.kind() == "enum_body_declarations" {
                let mut c2 = m.walk();
                members.extend(m.named_children(&mut c2));
            } else {
                members.push(m);
            }
        }
        let mut pending_doc: Option<String> = None;
        for m in members {
            match m.kind() {
                "block_comment" => {
                    let t = text(m, b);
                    if t.starts_with("/**") {
                        pending_doc = Some(clean_javadoc(t));
                    }
                    continue;
                }
                "line_comment" => continue,
                "method_declaration" => decl.methods.push(method(m, b)),
                "field_declaration" | "constant_declaration" => {
                    let (anns, mods) = modifiers(m, b);
                    let is_static = kind == TypeKind::Interface || mods.contains("static");
                    let is_const = kind == TypeKind::Interface
                        || (mods.contains("static") && mods.contains("final"));
                    let ty = m
                        .child_by_field_name("type")
                        .map(|t| text(t, b).to_string())
                        .unwrap_or_default();
                    let mut c2 = m.walk();
                    for d in m.children_by_field_name("declarator", &mut c2) {
                        let Some(n) = d.child_by_field_name("name") else {
                            continue;
                        };
                        let name = text(n, b).to_string();
                        if is_const && let Some(v) = d.child_by_field_name("value") {
                            decl.constants.insert(name.clone(), expr(v, b));
                        }
                        decl.fields.push(Field {
                            name,
                            ty: ty.clone(),
                            annotations: anns.clone(),
                            is_static,
                            is_transient: mods.contains("transient"),
                        });
                    }
                }
                "enum_constant" => {
                    let name = m
                        .child_by_field_name("name")
                        .map(|n| text(n, b).to_string())
                        .unwrap_or_default();
                    let args = m
                        .child_by_field_name("arguments")
                        .map(|a| args_of(a, b))
                        .unwrap_or_default();
                    let (anns, _) = modifiers(m, b);
                    decl.enum_constants.push(EnumConstant {
                        name,
                        args,
                        doc: pending_doc.take(),
                        annotations: anns,
                    });
                }
                "class_declaration"
                | "interface_declaration"
                | "enum_declaration"
                | "record_declaration"
                | "annotation_type_declaration" => nested.push(m),
                _ => {}
            }
            pending_doc = None;
        }

        let idx = self.types.len();
        self.by_fqn.insert(fqn.clone(), idx);
        self.by_simple.entry(simple_name).or_default().push(idx);
        self.types.push(decl);

        for n in nested {
            self.collect_type(n, b, package, Some(&fqn), path, file_idx);
        }
    }

    pub fn by_fqn(&self, fqn: &str) -> Option<&TypeDecl> {
        self.by_fqn.get(fqn).map(|&i| &self.types[i])
    }

    pub fn by_simple(&self, simple: &str) -> Vec<&TypeDecl> {
        self.by_simple
            .get(simple)
            .map(|v| v.iter().map(|&i| &self.types[i]).collect())
            .unwrap_or_default()
    }

    /// Resolves a type name as seen from `from` (imports, same package, nested, unique simple
    /// name).
    pub fn resolve_type(&self, from: &TypeDecl, name: &str) -> Option<&TypeDecl> {
        let file = &self.files[from.file_idx];
        if name.contains('.') {
            if let Some(t) = self.by_fqn(name) {
                return Some(t);
            }
            // Outer.Inner where Outer is imported or in the same package.
            let (head, tail) = name.split_once('.').unwrap();
            if let Some(outer) = self.resolve_type(from, head) {
                return self.by_fqn(&format!("{}.{tail}", outer.fqn));
            }
            return None;
        }
        // Nested in the declaring type (or its outer types).
        let mut scope = from.fqn.as_str();
        loop {
            if let Some(t) = self.by_fqn(&format!("{scope}.{name}")) {
                return Some(t);
            }
            match scope.rsplit_once('.') {
                Some((outer, _)) if outer != file.package => scope = outer,
                _ => break,
            }
        }
        if let Some(fqn) = file.imports.get(name)
            && let Some(t) = self.by_fqn(fqn)
        {
            return Some(t);
        }
        if let Some(t) = self.by_fqn(&format!("{}.{name}", file.package)) {
            return Some(t);
        }
        let candidates = self.by_simple(name);
        if candidates.len() == 1 {
            return Some(candidates[0]);
        }
        None
    }
}

fn first_named<'a>(node: Node<'a>, kinds: &[&str]) -> Option<Node<'a>> {
    let mut c = node.walk();
    node.named_children(&mut c)
        .find(|n| kinds.contains(&n.kind()))
}

fn find_child<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
    first_named(node, &[kind])
}

pub fn text<'a>(node: Node, b: &'a [u8]) -> &'a str {
    node.utf8_text(b).unwrap_or("")
}

/// Strips generic arguments and annotations: `Foo<Bar, Baz>` -> `Foo`.
pub fn erase(t: &str) -> String {
    let t = t.split('<').next().unwrap_or(t).trim();
    t.rsplit(' ').next().unwrap_or(t).to_string()
}

fn modifiers(node: Node, b: &[u8]) -> (Vec<Annotation>, String) {
    let mut anns = Vec::new();
    let mut mods = String::new();
    let mut c = node.walk();
    for ch in node.children(&mut c) {
        if ch.kind() == "modifiers" {
            let mut c2 = ch.walk();
            for m in ch.children(&mut c2) {
                match m.kind() {
                    "annotation" | "marker_annotation" => anns.push(annotation(m, b)),
                    _ => {
                        mods.push_str(text(m, b));
                        mods.push(' ');
                    }
                }
            }
        }
    }
    (anns, mods)
}

fn annotation(node: Node, b: &[u8]) -> Annotation {
    let name = node
        .child_by_field_name("name")
        .map(|n| text(n, b))
        .unwrap_or("");
    let name = name.rsplit('.').next().unwrap_or(name).to_string();
    let mut args = Vec::new();
    if let Some(list) = node.child_by_field_name("arguments") {
        let mut c = list.walk();
        for a in list.named_children(&mut c) {
            match a.kind() {
                "element_value_pair" => {
                    let k = a
                        .child_by_field_name("key")
                        .map(|k| text(k, b).to_string())
                        .unwrap_or_default();
                    let v = a
                        .child_by_field_name("value")
                        .map(|v| expr(v, b))
                        .unwrap_or(Expr::Other(String::new()));
                    args.push((k, v));
                }
                "line_comment" | "block_comment" => {}
                _ => args.push(("value".into(), expr(a, b))),
            }
        }
    }
    // `@OpenApi.Document` style names keep the qualifier in Java; keep the last segment, but
    // remember the qualifier for annotations that need it.
    Annotation { name, args }
}

fn method(node: Node, b: &[u8]) -> Method {
    let (annotations, _) = modifiers(node, b);
    let name = node
        .child_by_field_name("name")
        .map(|n| text(n, b).to_string())
        .unwrap_or_default();
    let mut params = Vec::new();
    if let Some(ps) = node.child_by_field_name("parameters") {
        let mut c = ps.walk();
        for p in ps.named_children(&mut c) {
            if p.kind() != "formal_parameter" && p.kind() != "spread_parameter" {
                continue;
            }
            let (anns, _) = modifiers(p, b);
            let ty = p
                .child_by_field_name("type")
                .map(|t| text(t, b).to_string())
                .or_else(|| {
                    let mut c2 = p.walk();
                    p.named_children(&mut c2)
                        .find(|n| n.kind().ends_with("_type") || n.kind() == "type_identifier")
                        .map(|t| text(t, b).to_string())
                })
                .unwrap_or_default();
            let pname = p
                .child_by_field_name("name")
                .map(|n| text(n, b).to_string())
                .or_else(|| {
                    find_child(p, "variable_declarator")
                        .and_then(|d| d.child_by_field_name("name"))
                        .map(|n| text(n, b).to_string())
                })
                .unwrap_or_default();
            params.push(Param {
                annotations: anns,
                ty,
                name: pname,
            });
        }
    }
    let body_node = node.child_by_field_name("body");
    let mut returns = Vec::new();
    if let Some(bn) = body_node {
        collect_returns(bn, b, &mut returns);
    }
    Method {
        name,
        annotations,
        params,
        line: node.start_position().row + 1,
        body: body_node.map(|n| text(n, b).to_string()),
        returns,
    }
}

fn collect_returns(node: Node, b: &[u8], out: &mut Vec<Expr>) {
    if node.kind() == "return_statement" {
        if let Some(e) = node.named_child(0) {
            out.push(expr(e, b));
        }
        return;
    }
    // Do not descend into lambdas or nested classes.
    if matches!(node.kind(), "lambda_expression" | "class_body") {
        return;
    }
    let mut c = node.walk();
    for ch in node.named_children(&mut c) {
        collect_returns(ch, b, out);
    }
}

fn args_of(list: Node, b: &[u8]) -> Vec<Expr> {
    let mut c = list.walk();
    list.named_children(&mut c)
        .filter(|n| !matches!(n.kind(), "line_comment" | "block_comment"))
        .map(|n| expr(n, b))
        .collect()
}

pub fn expr(node: Node, b: &[u8]) -> Expr {
    match node.kind() {
        "string_literal" => Expr::Str(unescape_java(text(node, b))),
        "decimal_integer_literal" => text(node, b)
            .replace('_', "")
            .trim_end_matches(['l', 'L'])
            .parse()
            .map(Expr::Int)
            .unwrap_or_else(|_| Expr::Other(text(node, b).into())),
        "true" => Expr::Bool(true),
        "false" => Expr::Bool(false),
        "identifier" => Expr::Name(vec![text(node, b).to_string()]),
        "scoped_identifier" | "field_access" => {
            let t: String = text(node, b)
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect();
            if t.chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '.' || c == '$')
            {
                Expr::Name(t.split('.').map(str::to_string).collect())
            } else {
                Expr::Other(t)
            }
        }
        "parenthesized_expression" => node
            .named_child(0)
            .map(|n| expr(n, b))
            .unwrap_or(Expr::Other(String::new())),
        "binary_expression" => {
            let op = node
                .child_by_field_name("operator")
                .map(|o| text(o, b))
                .unwrap_or("");
            match (
                op,
                node.child_by_field_name("left"),
                node.child_by_field_name("right"),
            ) {
                ("+", Some(l), Some(r)) => {
                    let mut parts = Vec::new();
                    for side in [expr(l, b), expr(r, b)] {
                        match side {
                            Expr::Concat(inner) => parts.extend(inner),
                            other => parts.push(other),
                        }
                    }
                    Expr::Concat(parts)
                }
                _ => Expr::Other(text(node, b).into()),
            }
        }
        "element_value_array_initializer" | "array_initializer" => Expr::Array(args_of(node, b)),
        "array_creation_expression" => node
            .child_by_field_name("value")
            .map(|v| expr(v, b))
            .unwrap_or(Expr::Other(text(node, b).into())),
        "class_literal" => Expr::ClassLit(
            node.named_child(0)
                .map(|t| erase(text(t, b)))
                .unwrap_or_default(),
        ),
        "method_invocation" => Expr::Call {
            target: node
                .child_by_field_name("object")
                .map(|o| Box::new(expr(o, b))),
            name: node
                .child_by_field_name("name")
                .map(|n| text(n, b).to_string())
                .unwrap_or_default(),
            args: node
                .child_by_field_name("arguments")
                .map(|a| args_of(a, b))
                .unwrap_or_default(),
        },
        "annotation" | "marker_annotation" => Expr::Annotation(Box::new(annotation(node, b))),
        _ => Expr::Other(text(node, b).into()),
    }
}

/// Decodes a Java string literal (including the quotes), or a text block.
pub fn unescape_java(lit: &str) -> String {
    let inner = if let Some(s) = lit.strip_prefix("\"\"\"") {
        s.strip_suffix("\"\"\"")
            .unwrap_or(s)
            .trim_start_matches(['\r', '\n'])
    } else {
        lit.strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .unwrap_or(lit)
    };
    let mut out = String::new();
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('b') => out.push('\u{8}'),
            Some('f') => out.push('\u{c}'),
            Some('s') => out.push(' '),
            Some('0'..='7') => out.push('\0'),
            Some('u') => {
                while chars.peek() == Some(&'u') {
                    chars.next();
                }
                let hex: String = chars.by_ref().take(4).collect();
                if let Some(ch) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                    out.push(ch);
                }
            }
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

/// Collapses a `/** ... */` comment into one line of text.
pub fn clean_javadoc(t: &str) -> String {
    let body = t.trim_start_matches("/**").trim_end_matches("*/");
    body.lines()
        .map(|l| l.trim().trim_start_matches('*').trim())
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Walks a directory tree of Java sources, skipping test sources.
pub fn main_java_files(root: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = walkdir::WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| {
            let n = e.file_name().to_string_lossy();
            !(e.file_type().is_dir()
                && (n == "test" || n == "target" || n == "node_modules" || n.starts_with('.')))
        })
        .filter_map(Result::ok)
        .filter(|e| {
            e.file_type().is_file()
                && e.path().extension().is_some_and(|x| x == "java")
                && e.path().components().any(|c| c.as_os_str() == "main")
        })
        .map(|e| e.into_path())
        .collect();
    out.sort();
    out
}
