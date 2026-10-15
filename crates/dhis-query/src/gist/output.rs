//! `GistOutput`/`JsonStreamOutput` JSON as json-tree's `JsonBuilder` (1.9.5) writes it, and the
//! pager links of `DefaultGistService#pager`.
//!
//! json-tree differs from Jackson: it escapes only control characters, `"`, `\` and U+2028/9
//! (`\uXXXX` upper-case unless a short form exists), and leaves null members out. Doubles are
//! written with `Double.toString`, except inside single-field lists (`unwrap`), whose values are
//! first written to a `TextBuilder` with its own (lossy) formatting, see [`text_builder_double`].

use dhis_core_model::java::json::double_to_string;

use super::plan::{ListPlan, OutputType, Slot};
use super::{Result, Unsupported};

/// A value of a fetched row.
#[derive(Debug, Clone, PartialEq)]
pub enum Cell {
    Null,
    Text(String),
    Double(f64),
    Int(i64),
    Bool(bool),
    Map(Vec<(String, String)>),
}

/// `GistPager`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pager {
    pub page: i32,
    pub page_size: i32,
    pub total: Option<i32>,
    pub prev_page: Option<String>,
    pub next_page: Option<String>,
}

impl Pager {
    /// `GistPager.getPageCount`: `ceil(total / (double) pageSize)`.
    pub fn page_count(&self) -> Option<i32> {
        self.total
            .map(|t| (f64::from(t) / f64::from(self.page_size)).ceil() as i32)
    }
}

/// How the list is wrapped (`GistObjectList.Output`).
#[derive(Debug, Clone)]
pub struct ListOutput<'a> {
    pub collection_name: &'a str,
    pub headless: bool,
    pub unwrap: bool,
}

/// `JsonAppender#appendEscaped`.
pub fn write_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 || c == '\u{2028}' || c == '\u{2029}' => {
                out.push_str(&format!("\\u{:04X}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// json-tree `TextBuilder#append(double)`: a shortcut for up to three decimals that drops the
/// sign of values between -1 and 0 and leading zeros of the decimals (0.05 prints as `0.5`);
/// reproduced as is.
pub fn text_builder_double(value: f64) -> String {
    if value.is_nan() {
        return "NaN".into();
    }
    if value == f64::INFINITY {
        return "Infinity".into();
    }
    if value == f64::NEG_INFINITY {
        return "-Infinity".into();
    }
    if value == 0.0 {
        return if value.is_sign_negative() {
            "-0.0"
        } else {
            "0.0"
        }
        .into();
    }
    let decimal = |int: i64, dec: i64| {
        let mut d = dec;
        while d > 10 && d % 10 == 0 {
            d /= 10;
        }
        format!("{int}.{d}")
    };
    let abs = value.abs();
    if (1e-3..1e7).contains(&abs) {
        if value % 1.0 == 0.0 {
            return format!("{}.0", value as i64);
        }
        let scaled = value * 10.0;
        if scaled % 1.0 == 0.0 {
            if scaled / 10.0 == value {
                let v = scaled as i64;
                return decimal(v / 10, (v % 10).abs());
            }
        } else {
            let scaled = value * 100.0;
            if scaled % 1.0 == 0.0 {
                if scaled / 100.0 == value {
                    let v = scaled as i64;
                    return decimal(v / 100, (v % 100).abs());
                }
            } else {
                let scaled = value * 1000.0;
                if scaled % 1.0 == 0.0 && scaled / 1000.0 == value {
                    let v = scaled as i64;
                    return decimal(v / 1000, (v % 1000).abs());
                }
            }
        }
    }
    double_to_string(value)
}

/// Fills the computed slots (`GistBuilder#transform`).
fn transform(plan: &ListPlan, row: &mut [Cell]) {
    for i in 0..plan.slots.len() {
        match &plan.slots[i] {
            Slot::Href {
                id_slot,
                prefix,
                suffix,
            } => {
                row[i] = match (id_slot, prefix) {
                    (Some(id), Some(prefix)) => match &row[*id] {
                        Cell::Text(id) => Cell::Text(format!("{prefix}{id}{suffix}")),
                        _ => Cell::Null,
                    },
                    _ => Cell::Null,
                };
            }
            Slot::Refs { entries } if !entries.is_empty() => {
                let map = entries
                    .iter()
                    .map(|e| {
                        // Java concatenates the id object (`null` prints as "null")
                        let id = match &row[e.id_slot] {
                            Cell::Text(t) => t.clone(),
                            _ => "null".to_string(),
                        };
                        (e.key.clone(), format!("{}{id}{}", e.prefix, e.suffix))
                    })
                    .collect();
                row[i] = Cell::Map(map);
            }
            _ => {}
        }
    }
}

/// Appends one value as the property adders do; `false` for a null (left out).
fn write_value(out: &mut String, ty: OutputType, cell: &Cell, unwrapped: bool) -> Result<bool> {
    match (ty, cell) {
        (_, Cell::Null) => return Ok(false),
        (OutputType::String | OutputType::Date, Cell::Text(s)) => write_string(out, s),
        (OutputType::Double, Cell::Double(d)) => {
            let text = if unwrapped {
                text_builder_double(*d)
            } else {
                double_to_string(*d)
            };
            // JsonBuilder.requiresString: NaN and infinities become JSON strings
            if d.is_finite() {
                out.push_str(&text);
            } else {
                out.push('"');
                out.push_str(&text);
                out.push('"');
            }
        }
        (OutputType::Integer, Cell::Int(i)) => out.push_str(&i.to_string()),
        (OutputType::Boolean, Cell::Bool(b)) => out.push_str(if *b { "true" } else { "false" }),
        (OutputType::StringMap, Cell::Map(m)) => {
            out.push('{');
            for (i, (k, v)) in m.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push('"');
                out.push_str(k);
                out.push_str("\":");
                write_string(out, v);
            }
            out.push('}');
        }
        _ => return Err(Unsupported("value of unexpected type")),
    }
    Ok(true)
}

/// Renders a list response (`GistOutput.toJson(GistObjectList.Output)`).
pub fn render_list(
    plan: &ListPlan,
    mut rows: Vec<Vec<Cell>>,
    pager: &Pager,
    wrap: &ListOutput<'_>,
) -> Result<String> {
    for row in rows.iter_mut() {
        if row.len() != plan.slots.len() {
            return Err(Unsupported("row width"));
        }
        // a query of one column yields the value itself as the row; a null row fails in Java
        if row.len() == 1 && row[0] == Cell::Null {
            return Err(Unsupported("null single-column row"));
        }
        transform(plan, row);
    }
    let unwrap = wrap.unwrap && plan.output.len() == 1;
    let mut elements = String::from("[");
    for (n, row) in rows.iter().enumerate() {
        if n > 0 {
            elements.push(',');
        }
        if unwrap {
            // each element is the value itself; a missing (null) member fails in Java
            if !write_value(&mut elements, plan.output[0].ty, &row[0], true)? {
                return Err(Unsupported("null value in unwrapped list"));
            }
            continue;
        }
        elements.push('{');
        let mut first = true;
        for (i, prop) in plan.output.iter().enumerate() {
            let mut member = String::new();
            member.push('"');
            member.push_str(&prop.path);
            member.push_str("\":");
            if write_value(&mut member, prop.ty, &row[i], false)? {
                if !first {
                    elements.push(',');
                }
                first = false;
                elements.push_str(&member);
            }
        }
        elements.push('}');
    }
    elements.push(']');
    if wrap.headless {
        return Ok(elements);
    }
    let mut out = String::from("{\"pager\":{");
    out.push_str(&format!(
        "\"page\":{},\"pageSize\":{}",
        pager.page, pager.page_size
    ));
    if let Some(t) = pager.total {
        out.push_str(&format!(",\"total\":{t}"));
    }
    if let Some(c) = pager.page_count() {
        out.push_str(&format!(",\"pageCount\":{c}"));
    }
    if let Some(p) = &pager.prev_page {
        out.push_str(",\"prevPage\":");
        write_string(&mut out, p);
    }
    if let Some(n) = &pager.next_page {
        out.push_str(",\"nextPage\":");
        write_string(&mut out, n);
    }
    out.push_str("},\"gist\":true,\"");
    out.push_str(wrap.collection_name);
    out.push_str("\":");
    out.push_str(&elements);
    out.push('}');
    Ok(out)
}

/// The request URL as `ContextUtils#getRequestURL` builds it (Tomcat's scheme, server name and
/// port, raw request URI and query), already split.
#[derive(Debug, Clone)]
pub struct RequestUrl<'a> {
    pub scheme: &'a str,
    pub host: &'a str,
    /// Omitted when it is the scheme's default.
    pub port: Option<u16>,
    pub path: &'a str,
    pub query: Option<&'a str>,
}

fn is_java_uri_host(host: &str) -> bool {
    // java.net.URI#parseServer: an IPv4 address or a hostname whose top label starts with a
    // letter; anything else makes the authority registry-based (no host), not reproduced here.
    let ipv4 = host.split('.').count() == 4
        && host.split('.').all(|p| {
            !p.is_empty()
                && p.len() <= 3
                && p.bytes().all(|b| b.is_ascii_digit())
                && p.parse::<u16>().is_ok_and(|n| n <= 255)
        });
    if ipv4 {
        return true;
    }
    let h = host.strip_suffix('.').unwrap_or(host);
    let labels: Vec<&str> = h.split('.').collect();
    let label_ok = |l: &str| {
        let b = l.as_bytes();
        !b.is_empty()
            && b[0].is_ascii_alphanumeric()
            && b[b.len() - 1].is_ascii_alphanumeric()
            && b.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'-')
    };
    labels.iter().all(|l| label_ok(l))
        && labels
            .last()
            .is_some_and(|l| l.as_bytes()[0].is_ascii_alphabetic())
}

/// Characters `java.net.URI` accepts unescaped in a path (`extra` adds the query-only ones).
fn is_java_uri_text(s: &str, extra: &[u8]) -> bool {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == b'%' {
            if i + 2 < b.len() && b[i + 1].is_ascii_hexdigit() && b[i + 2].is_ascii_hexdigit() {
                i += 3;
                continue;
            }
            return false;
        }
        let ok = c.is_ascii_alphanumeric()
            || b"-_.!~*'()".contains(&c)
            || b";:@&=+$,/".contains(&c)
            || extra.contains(&c);
        if !ok {
            return false;
        }
        i += 1;
    }
    true
}

/// `UriComponentsBuilder.fromUri(URI.create(requestURL)).replaceQueryParam("page", page)
/// .build().toString()`.
pub fn page_link(url: &RequestUrl<'_>, page: i32) -> Result<String> {
    if !is_java_uri_host(url.host) {
        return Err(Unsupported("request host"));
    }
    if !is_java_uri_text(url.path, b"") {
        return Err(Unsupported("request path characters"));
    }
    if let Some(q) = url.query
        && !is_java_uri_text(q, b"?[]")
    {
        return Err(Unsupported("request query characters"));
    }
    let mut out = format!("{}://{}", url.scheme, url.host);
    if let Some(p) = url.port {
        out.push_str(&format!(":{p}"));
    }
    // FullPathComponentBuilder: "//" collapses to "/"
    let mut path = url.path.to_string();
    while let Some(i) = path.find("//") {
        path.remove(i);
    }
    out.push_str(&path);
    // QUERY_PARAM_PATTERN "([^&=]+)(=?)([^&]+)?" into a LinkedMultiValueMap (grouped by name in
    // first-seen order); `page` is then replaced by one value at the end.
    let mut params: Vec<(String, Vec<Option<String>>)> = Vec::new();
    if let Some(q) = url.query.filter(|q| !q.is_empty()) {
        let b = q.as_bytes();
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'&' || b[i] == b'=' {
                i += 1;
                continue;
            }
            let start = i;
            while i < b.len() && b[i] != b'&' && b[i] != b'=' {
                i += 1;
            }
            let name = &q[start..i];
            let mut eq = false;
            if i < b.len() && b[i] == b'=' {
                eq = true;
                i += 1;
            }
            let vstart = i;
            while i < b.len() && b[i] != b'&' {
                i += 1;
            }
            let value = if i > vstart {
                Some(q[vstart..i].to_string())
            } else if eq {
                Some(String::new())
            } else {
                None
            };
            match params.iter_mut().find(|(n, _)| n == name) {
                Some((_, vs)) => vs.push(value),
                None => params.push((name.to_string(), vec![value])),
            }
        }
    }
    params.retain(|(n, _)| n != "page");
    params.push(("page".into(), vec![Some(page.to_string())]));
    let mut query = String::new();
    for (name, values) in &params {
        for v in values {
            if !query.is_empty() {
                query.push('&');
            }
            query.push_str(name);
            if let Some(v) = v {
                query.push('=');
                query.push_str(v);
            }
        }
    }
    out.push('?');
    out.push_str(&query);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_tree_escaping() {
        let mut s = String::new();
        write_string(&mut s, "a\"b\\c\n\u{1}\u{7f}<&>é\u{2028}/");
        assert_eq!(s, "\"a\\\"b\\\\c\\n\\u0001\u{7f}<&>é\\u2028/\"");
    }

    /// json-tree's own output (`tools/gen-json-tree-fixtures.sh`, server image): bits, the
    /// `TextBuilder` form, the streamed form.
    #[test]
    fn doubles_like_json_tree() {
        let fixtures = include_str!("../../tests/fixtures/json-tree-doubles.tsv");
        let mut n = 0;
        for line in fixtures.lines() {
            let mut cols = line.split('\t');
            let bits: i64 = cols.next().unwrap().parse().unwrap();
            let v = f64::from_bits(bits as u64);
            let (tb, streamed) = (cols.next().unwrap(), cols.next().unwrap());
            // NaN/Infinity are quoted by json-tree; Gist never sees them from PostgreSQL doubles
            // without them being stored, and both forms agree on the text.
            let tb = tb.trim_matches('"');
            let streamed = streamed.trim_matches('"');
            assert_eq!(text_builder_double(v), tb, "TextBuilder {v:e}");
            assert_eq!(double_to_string(v), streamed, "streamed {v:e}");
            n += 1;
        }
        assert_eq!(n, 441);
        assert_eq!(text_builder_double(0.05), "0.5");
    }

    #[test]
    fn page_links_like_spring() {
        let u = |q: &'static str| RequestUrl {
            scheme: "http",
            host: "localhost",
            port: Some(8081),
            path: "/api/constants/gist",
            query: Some(q),
        };
        assert_eq!(
            page_link(&u("pageSize=2&page=2&fields=id&x=a%20b&page=7"), 3).unwrap(),
            "http://localhost:8081/api/constants/gist?pageSize=2&fields=id&x=a%20b&page=3"
        );
        assert_eq!(
            page_link(&u("pageSize=1&fields=id&x=&&y=1&x=2"), 2).unwrap(),
            "http://localhost:8081/api/constants/gist?pageSize=1&fields=id&x=&x=2&y=1&page=2"
        );
        assert_eq!(
            page_link(&u("fields=id&x"), 2).unwrap(),
            "http://localhost:8081/api/constants/gist?fields=id&x&page=2"
        );
        assert_eq!(
            page_link(&u("filter=id:in:[a,b]"), 2).unwrap(),
            "http://localhost:8081/api/constants/gist?filter=id:in:[a,b]&page=2"
        );
        assert!(page_link(&u("x=a|b"), 2).is_err());
        assert!(page_link(&u("x=%zz"), 2).is_err());
        let no_query = RequestUrl {
            scheme: "http",
            host: "foo",
            port: None,
            path: "/api/constants",
            query: None,
        };
        assert_eq!(
            page_link(&no_query, 2).unwrap(),
            "http://foo/api/constants?page=2"
        );
        assert!(
            page_link(
                &RequestUrl {
                    host: "my_host",
                    ..no_query.clone()
                },
                2
            )
            .is_err()
        );
    }
}
