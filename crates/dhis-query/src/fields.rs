//! Port of `org.hisp.dhis.common.input.Fields` (dhis-api): the `fields=` URL mini-language.
//!
//! ```text
//! fields     = field ("," field)*            empty fields and a dangling "," are ignored
//! field      = name transform* children?
//! name       = [:*!-]* [a-zA-Z0-9_.-]+       presets (":all", "*") and excludes ("!x", "-x")
//! transform  = (":"|"~"|"@"|"|"){1,2} name ("(" name ([,;] name)* ")")?
//! children   = ("[" | "(") fields ("]" | ")")?   the closing bracket may be omitted at the end
//! ```
//!
//! Two views of the parse result, as in Java:
//! - [`parse`] (`Fields.parse`, used by `FieldFilterParser`): a flat, de-duplicated list of
//!   [`FieldPath`]s including the parent of every nested block;
//! - [`Fields::of`]: leaf [`Field`]s with renames resolved, for the newer field filtering.

use crate::IllegalArgument;
use crate::property_path::PropertyPath;

/// Exceptions `Fields.of` can throw; they map to different HTTP responses.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FieldsError {
    #[error(transparent)]
    IllegalArgument(#[from] IllegalArgument),
    /// `IndexOutOfBoundsException`, e.g. `x::rename` without an argument (`args.get(0)`).
    #[error("{0}")]
    IndexOutOfBounds(String),
}

/// `org.hisp.dhis.fieldfiltering.FieldPathTransformer`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldPathTransformer {
    pub name: String,
    pub parameters: Vec<String>,
}

/// `org.hisp.dhis.fieldfiltering.FieldPath`. Equality, like Java, only compares the path.
#[derive(Debug, Clone)]
pub struct FieldPath {
    pub path: PropertyPath,
    pub transformers: Vec<FieldPathTransformer>,
}

impl PartialEq for FieldPath {
    fn eq(&self, other: &Self) -> bool {
        self.path == other.path
    }
}

impl Eq for FieldPath {}

/// `Gist.Transform`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transform {
    Auto,
    None,
    Translate,
    Attribute,
    IsEmpty,
    IsNotEmpty,
    Size,
    Member,
    NotMember,
    Ids,
    IdObjects,
    Pluck,
    From,
}

impl Transform {
    const ALL: [(Transform, &'static str); 13] = [
        (Transform::Auto, "AUTO"),
        (Transform::None, "NONE"),
        (Transform::Translate, "TRANSLATE"),
        (Transform::Attribute, "ATTRIBUTE"),
        (Transform::IsEmpty, "IS_EMPTY"),
        (Transform::IsNotEmpty, "IS_NOT_EMPTY"),
        (Transform::Size, "SIZE"),
        (Transform::Member, "MEMBER"),
        (Transform::NotMember, "NOT_MEMBER"),
        (Transform::Ids, "IDS"),
        (Transform::IdObjects, "ID_OBJECTS"),
        (Transform::Pluck, "PLUCK"),
        (Transform::From, "FROM"),
    ];

    /// `Transform.of(String)`: name without `_` equals input without `-`, ignoring case;
    /// unknown names map to `AUTO`.
    pub fn of(name: &str) -> Transform {
        let wanted = name.replace('-', "");
        Self::ALL
            .iter()
            .find(|(_, n)| n.replace('_', "").eq_ignore_ascii_case(&wanted))
            .map(|(t, _)| *t)
            .unwrap_or(Transform::Auto)
    }
}

/// `Fields.Field`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub property_path: PropertyPath,
    pub renamed_path: Option<PropertyPath>,
    pub transformation: Transform,
    pub args: Vec<String>,
}

impl Field {
    fn new(property_path: PropertyPath) -> Self {
        Self {
            property_path,
            renamed_path: None,
            transformation: Transform::Auto,
            args: Vec::new(),
        }
    }

    /// The path rendered in the output.
    pub fn path(&self) -> &PropertyPath {
        self.renamed_path.as_ref().unwrap_or(&self.property_path)
    }
}

/// `Fields`: leaf fields of a `fields=` expression.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Fields(pub Vec<Field>);

impl Fields {
    /// `Fields.of(String)`.
    pub fn of(fields: &str) -> Result<Fields, FieldsError> {
        if fields.is_empty() {
            return Ok(Fields::default());
        }
        let text = Text::new(fields);
        let mut res = Vec::new();
        parse_fields(&text, 0, &mut res)?;
        let mut out = Vec::new();
        for e in &res {
            e.to_fields(None, None, &mut out)?;
        }
        Ok(Fields(out))
    }
}

/// `Fields.parse(String)`: the legacy flat list of field paths (distinct by path, first wins).
pub fn parse(fields: &str) -> Result<Vec<FieldPath>, IllegalArgument> {
    if fields.is_empty() {
        return Ok(Vec::new());
    }
    let text = Text::new(fields);
    let mut res = Vec::new();
    parse_fields(&text, 0, &mut res)?;
    let mut out: Vec<FieldPath> = Vec::new();
    for e in &res {
        let mut paths = Vec::new();
        e.to_field_paths(None, &mut paths)?;
        for p in paths {
            if !out.contains(&p) {
                out.push(p);
            }
        }
    }
    Ok(out)
}

/// The input as UTF-16 units, so positions in error messages match Java.
struct Text {
    units: Vec<u16>,
    original: String,
}

impl Text {
    fn new(s: &str) -> Self {
        Self {
            units: s.encode_utf16().collect(),
            original: s.to_string(),
        }
    }
    fn len(&self) -> usize {
        self.units.len()
    }
    /// The unit as a char when ASCII, otherwise a non-matching placeholder.
    fn at(&self, i: usize) -> char {
        char::from_u32(u32::from(self.units[i])).unwrap_or('\u{FFFD}')
    }
    fn slice(&self, a: usize, b: usize) -> String {
        String::from_utf16_lossy(&self.units[a..b])
    }
}

struct TransformExp {
    ty: String,
    args: Vec<String>,
}

struct FieldExp {
    name: String,
    transforms: Vec<TransformExp>,
    children: Vec<FieldExp>,
}

fn is_exclude_marker(c: char) -> bool {
    c == '!' || c == '-'
}

fn is_preset_marker(c: char) -> bool {
    c == ':' || c == '*'
}

impl FieldExp {
    /// Drops the negation of a preset (`!:all` → `:all`, `!*` → `*`).
    fn unified_name(&self) -> &str {
        let mut cs = self.name.chars();
        match (cs.next(), cs.next()) {
            (Some(a), Some(b)) if is_exclude_marker(a) && is_preset_marker(b) => &self.name[1..],
            _ => &self.name,
        }
    }

    fn to_field_paths(
        &self,
        parent: Option<&PropertyPath>,
        out: &mut Vec<FieldPath>,
    ) -> Result<(), IllegalArgument> {
        let path = PropertyPath::concat_opt(parent, self.unified_name())?;
        let transformers = self
            .transforms
            .iter()
            .map(|t| FieldPathTransformer {
                name: t.ty.clone(),
                parameters: t.args.clone(),
            })
            .collect();
        out.push(FieldPath {
            path: path.clone(),
            transformers,
        });
        for c in &self.children {
            c.to_field_paths(Some(&path), out)?;
        }
        Ok(())
    }

    fn to_fields(
        &self,
        parent: Option<&PropertyPath>,
        parent_renamed: Option<&PropertyPath>,
        out: &mut Vec<Field>,
    ) -> Result<(), FieldsError> {
        let name = self.unified_name();
        let mut f = Field::new(PropertyPath::concat_opt(parent, name)?);
        // Java: `for (t : transforms) if (rename) renamedName = t.args.get(0);` (last wins).
        let mut renamed_name: Option<&String> = None;
        for t in self.transforms.iter().filter(|t| t.ty == "rename") {
            renamed_name = Some(t.args.first().ok_or_else(|| {
                FieldsError::IndexOutOfBounds("Index 0 out of bounds for length 0".into())
            })?);
        }
        if renamed_name.is_some() || parent_renamed.is_some() {
            let base = parent_renamed.or(parent);
            f.renamed_path = Some(PropertyPath::concat_opt(
                base,
                renamed_name.map(String::as_str).unwrap_or(name),
            )?);
        }
        if self.transforms.is_empty() && self.children.is_empty() {
            out.push(f);
            return Ok(());
        }
        let renamed = f.renamed_path.is_some();
        let no_transform = self.transforms.is_empty() || (renamed && self.transforms.len() == 1);
        let transformed: Vec<Field> = self
            .transforms
            .iter()
            .filter(|t| t.ty != "rename")
            .map(|t| Field {
                transformation: Transform::of(&t.ty),
                args: t.args.clone(),
                ..f.clone()
            })
            .collect();
        if self.children.is_empty() {
            if no_transform {
                out.push(f);
            } else {
                out.extend(transformed);
            }
            return Ok(());
        }
        if !no_transform {
            out.extend(transformed);
        }
        for c in &self.children {
            c.to_fields(Some(&f.property_path), f.renamed_path.as_ref(), out)?;
        }
        Ok(())
    }
}

fn parse_fields(
    t: &Text,
    offset: usize,
    res: &mut Vec<FieldExp>,
) -> Result<usize, IllegalArgument> {
    let mut i = offset;
    let nested = offset > 0;
    let len = t.len();
    while i < len {
        i = parse_field(t, i, nested, res)?;
        if i >= len {
            return Ok(i);
        }
        let c = t.at(i);
        if is_nested_close(c) {
            return Ok(i);
        }
        if c != ',' {
            return Err(expected_character(',', t, i));
        }
        i += 1;
    }
    Ok(i)
}

fn parse_field(
    t: &Text,
    offset: usize,
    nested: bool,
    res: &mut Vec<FieldExp>,
) -> Result<usize, IllegalArgument> {
    let len = t.len();
    let mut i = skip_space(t, offset);
    if i >= len {
        return Ok(i);
    }
    let c = t.at(i);
    if c == ',' || (nested && is_nested_close(c)) {
        return Ok(i);
    }
    let mut s = i;
    while s < len && is_name_marker(t.at(s)) {
        s += 1;
    }
    let e = parse_name(t, s);
    if e == i {
        return Err(expected_name_character(t, i));
    }
    let mut f = FieldExp {
        name: t.slice(i, e),
        transforms: Vec::new(),
        children: Vec::new(),
    };
    i = skip_space(t, e);
    if i >= len || t.at(i) == ',' {
        res.push(f);
        return Ok(i);
    }
    i = parse_transforms(t, i, &mut f.transforms)?;
    let r = parse_children(t, i, &mut f.children);
    res.push(f);
    r
}

fn parse_name(t: &Text, offset: usize) -> usize {
    let mut i = offset;
    while i < t.len() && is_name_character(t.at(i)) {
        i += 1;
    }
    i
}

fn parse_transforms(
    t: &Text,
    offset: usize,
    res: &mut Vec<TransformExp>,
) -> Result<usize, IllegalArgument> {
    let len = t.len();
    let mut i = offset;
    while i < len && is_transform_marker(t.at(i)) {
        let c = t.at(i);
        i += 1;
        if i < len && t.at(i) == c {
            i += 1;
        }
        let e = parse_name(t, i);
        if e == i {
            return Err(expected_name_character(t, i));
        }
        let mut te = TransformExp {
            ty: t.slice(i, e),
            args: Vec::new(),
        };
        i = parse_name_list(t, e, &mut te.args)?;
        res.push(te);
    }
    Ok(i)
}

fn parse_name_list(
    t: &Text,
    offset: usize,
    res: &mut Vec<String>,
) -> Result<usize, IllegalArgument> {
    let len = t.len();
    let mut i = offset;
    if i < len && t.at(i) == '(' {
        i += 1;
        while i < len && t.at(i) != ')' {
            let e = parse_name(t, i);
            if e == i {
                return Err(expected_name_character(t, i));
            }
            res.push(t.slice(i, e));
            i = e;
            if i >= len {
                return Ok(i);
            }
            let c = t.at(i);
            if c != ')' && !is_separator(c) {
                return Err(expected_character(',', t, i));
            }
            if is_separator(c) {
                i += 1;
            }
        }
        if i >= len {
            return Err(expected_character(')', t, i));
        }
        i += 1;
    }
    Ok(i)
}

fn parse_children(
    t: &Text,
    offset: usize,
    res: &mut Vec<FieldExp>,
) -> Result<usize, IllegalArgument> {
    let len = t.len();
    let mut i = offset;
    if i >= len || !is_nested_open(t.at(i)) {
        return Ok(i);
    }
    i += 1;
    i = skip_space(t, i);
    if i >= len {
        return Ok(i);
    }
    if is_nested_close(t.at(i)) {
        return Ok(i + 1);
    }
    i = parse_fields(t, i, res)?;
    if i >= len {
        return Ok(i);
    }
    if !is_nested_close(t.at(i)) {
        return Err(expected_character(']', t, i));
    }
    Ok(i + 1)
}

fn skip_space(t: &Text, offset: usize) -> usize {
    let mut i = offset;
    while i < t.len() && t.at(i) == ' ' {
        i += 1;
    }
    i
}

fn is_name_character(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.'
}

fn is_transform_marker(c: char) -> bool {
    matches!(c, ':' | '~' | '@' | '|')
}

fn is_name_marker(c: char) -> bool {
    is_preset_marker(c) || is_exclude_marker(c)
}

fn is_nested_open(c: char) -> bool {
    c == '[' || c == '('
}

fn is_nested_close(c: char) -> bool {
    c == ']' || c == ')'
}

fn is_separator(c: char) -> bool {
    c == ',' || c == ';'
}

fn but_found(t: &Text, offset: usize) -> String {
    if offset >= t.len() {
        "EOI".into()
    } else {
        String::from_utf16_lossy(&t.units[offset..offset + 1])
    }
}

/// `"%n  %s%n  %s"` with Java's `%n` (`\n` on the Linux servers DHIS2 runs on).
fn marker(t: &Text, offset: usize) -> String {
    format!("\n  {}\n  {}^", t.original, " ".repeat(offset))
}

fn expected_name_character(t: &Text, offset: usize) -> IllegalArgument {
    IllegalArgument(format!(
        "Expected a name character at position {offset} but found: {}{}",
        but_found(t, offset),
        marker(t, offset)
    ))
}

fn expected_character(ch: char, t: &Text, offset: usize) -> IllegalArgument {
    IllegalArgument(format!(
        "Expected {ch} at position {offset} but found: {}{}",
        but_found(t, offset),
        marker(t, offset)
    ))
}
