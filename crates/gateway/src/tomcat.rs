//! Tomcat 11.0.26 request-target validation, as configured by DHIS2
//! (`org.hisp.dhis.web.tomcat.Main`: `relaxedQueryChars = "\ { } | [ ]"`, no relaxed path chars).
//!
//! Requests Tomcat rejects with an HTML 400 before any servlet runs must not reach a Rust
//! handler; the gateway sends them to Java instead. Rules from `Http11InputBuffer#parseRequestLine`
//! and `Http11Processor#prepareRequest`:
//!
//! - bytes >= 0x80 are always invalid (Java reads them as negative and the lookup fails);
//! - after the first `?`, every byte must be a relaxed query char:
//!   unreserved, `%`, sub-delims, `:`, `@`, `/`, `?`, plus `\ { } | [ ]`;
//! - in the path, every byte must be an absolute-path char: unreserved, `%`, sub-delims, `:`,
//!   `@`, `/`.

fn is_unreserved(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~')
}

fn is_subdelim(b: u8) -> bool {
    matches!(
        b,
        b'!' | b'$' | b'&' | b'\'' | b'(' | b')' | b'*' | b'+' | b',' | b';' | b'='
    )
}

fn is_path_char(b: u8) -> bool {
    is_unreserved(b) || is_subdelim(b) || matches!(b, b'%' | b':' | b'@' | b'/')
}

fn is_query_char(b: u8) -> bool {
    is_path_char(b) || matches!(b, b'?' | b'\\' | b'{' | b'}' | b'|' | b'[' | b']')
}

/// Whether Tomcat would accept this origin-form request target (`path[?query]`).
pub fn is_valid_request_target(target: &str) -> bool {
    let bytes = target.as_bytes();
    let (path, query) = match bytes.iter().position(|b| *b == b'?') {
        Some(i) => (&bytes[..i], Some(&bytes[i + 1..])),
        None => (bytes, None),
    };
    path.iter().all(|b| *b < 0x80 && is_path_char(*b))
        && query.is_none_or(|q| q.iter().all(|b| *b < 0x80 && is_query_char(*b)))
}

/// Whether decoding the query parameters fails the way Tomcat's `Parameters#processParameters`
/// fails (malformed `%XX` escape, or bytes that are not UTF-8). Java then answers 409
/// ("Character decoding failed. Parameter [...] has been ignored. ...").
pub fn has_parameter_decode_failure(query: Option<&str>) -> bool {
    let Some(q) = query else { return false };
    q.split('&').any(|pair| {
        pair.split('=').any(|part| {
            let b = part.as_bytes();
            let mut out = Vec::with_capacity(b.len());
            let mut i = 0;
            while i < b.len() {
                match b[i] {
                    b'%' => {
                        let hex = |c: u8| (c as char).to_digit(16);
                        match (
                            b.get(i + 1).copied().and_then(hex),
                            b.get(i + 2).copied().and_then(hex),
                        ) {
                            (Some(h), Some(l)) => out.push((h * 16 + l) as u8),
                            _ => return true,
                        }
                        i += 3;
                    }
                    b'+' => {
                        out.push(b' ');
                        i += 1;
                    }
                    c => {
                        out.push(c);
                        i += 1;
                    }
                }
            }
            std::str::from_utf8(&out).is_err()
        })
    })
}

#[cfg(test)]
mod tests {
    use super::has_parameter_decode_failure as bad;
    use super::is_valid_request_target as ok;

    #[test]
    fn parameter_decoding() {
        assert!(!bad(None));
        assert!(!bad(Some("limit=3&x=%E2%82%AC&y=a+b")));
        assert!(bad(Some("limit=%zz")));
        assert!(bad(Some("limit=3%")));
        assert!(bad(Some("limit=2&x=%E2%82")));
        assert!(bad(Some("%zz=1")));
    }

    #[test]
    fn dhis2_style_queries_are_accepted() {
        assert!(ok("/api/me?fields=id,name,children[id,name]"));
        assert!(ok("/api/dataElements?filter=name:like:{x}|y\\z"));
        assert!(ok("/api/system/uid?limit=%D9%A3"));
        assert!(ok("/api/x?a=b?c"));
    }

    #[test]
    fn tomcat_rejections() {
        assert!(!ok("/api/system/uid?limit=\u{663}"));
        assert!(!ok("/api/a[0]"));
        assert!(!ok("/api/a{b}"));
        assert!(!ok("/api/a b"));
        assert!(!ok("/api/x?a=\"b\""));
        assert!(!ok("/api/x?a=<b>"));
        assert!(!ok("/api/x?a=`b`"));
        assert!(!ok("/api/x?a=^"));
        assert!(!ok("/api/x?a#b"));
    }
}
