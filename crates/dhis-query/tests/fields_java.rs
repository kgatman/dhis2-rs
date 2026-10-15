//! `fields=` parsing compared with the output of the Java implementation on the same inputs
//! (tests/fixtures/fields.tsv, generated with jshell from the pinned dhis-api jar).

use dhis_query::fields::{Fields, FieldsError, Transform, parse};

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\t', "\\t")
}

fn unesc(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('\\') => out.push('\\'),
                Some(o) => {
                    out.push('\\');
                    out.push(o)
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn transform_name(t: Transform) -> &'static str {
    match t {
        Transform::Auto => "AUTO",
        Transform::None => "NONE",
        Transform::Translate => "TRANSLATE",
        Transform::Attribute => "ATTRIBUTE",
        Transform::IsEmpty => "IS_EMPTY",
        Transform::IsNotEmpty => "IS_NOT_EMPTY",
        Transform::Size => "SIZE",
        Transform::Member => "MEMBER",
        Transform::NotMember => "NOT_MEMBER",
        Transform::Ids => "IDS",
        Transform::IdObjects => "ID_OBJECTS",
        Transform::Pluck => "PLUCK",
        Transform::From => "FROM",
    }
}

fn render_parse(input: &str) -> String {
    match parse(input) {
        Ok(paths) => paths
            .iter()
            .map(|p| {
                let t: String = p
                    .transformers
                    .iter()
                    .map(|t| format!("{{{}({})}}", t.name, t.parameters.join(";")))
                    .collect();
                format!("{}{t}", p.path)
            })
            .collect::<Vec<_>>()
            .join(","),
        Err(e) => format!("!IllegalArgumentException: {}", esc(&e.0)),
    }
}

fn render_of(input: &str) -> String {
    match Fields::of(input) {
        Ok(f) => {
            f.0.iter()
                .map(|f| {
                    format!(
                        "{}|{}|{}|{}",
                        f.property_path,
                        f.renamed_path
                            .as_ref()
                            .map(|p| p.to_string())
                            .unwrap_or_default(),
                        transform_name(f.transformation),
                        f.args.join(";")
                    )
                })
                .collect::<Vec<_>>()
                .join(",")
        }
        Err(FieldsError::IllegalArgument(e)) => format!("!IllegalArgumentException: {}", esc(&e.0)),
        Err(FieldsError::IndexOutOfBounds(m)) => format!("!IndexOutOfBoundsException: {}", esc(&m)),
    }
}

#[test]
fn matches_java_on_every_fixture() {
    let text = include_str!("fixtures/fields.tsv");
    let mut failures = Vec::new();
    let mut n = 0;
    for line in text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
    {
        let cols: Vec<&str> = line.split('\t').collect();
        let input = unesc(cols[0]);
        let (want_parse, want_of) = (
            cols.get(1).copied().unwrap_or(""),
            cols.get(2).copied().unwrap_or(""),
        );
        let (got_parse, got_of) = (render_parse(&input), render_of(&input));
        if got_parse != want_parse {
            failures.push(format!(
                "parse {input:?}\n  java: {want_parse}\n  rust: {got_parse}"
            ));
        }
        if got_of != want_of {
            failures.push(format!(
                "of    {input:?}\n  java: {want_of}\n  rust: {got_of}"
            ));
        }
        n += 1;
    }
    assert!(n > 90, "only {n} fixtures");
    assert!(
        failures.is_empty(),
        "{} mismatches:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn supplementary_chars_in_errors() {
    // Java prints the lone high surrogate of U+1D538 at position 2; a Rust String cannot hold
    // it, so it becomes U+FFFD (docs/deviations.md).
    let e = parse("a,\u{1D538}").unwrap_err().0;
    assert_eq!(
        e,
        "Expected a name character at position 2 but found: \u{FFFD}\n  a,\u{1D538}\n    ^"
    );
}
