//! Spring Security's default `StrictHttpFirewall` (spring-security-web 7.1.1; DHIS2 does not
//! configure its own), plus Tomcat's rejection of encoded slashes.
//!
//! Java rejects such requests with a 400 before any controller runs. Rust handlers must not
//! answer them, so the gateway sends them to Java. Over-matching is harmless (Java answers), so
//! the checks are applied to the raw path for both the encoded and the decoded block lists.

const ENCODED_BLOCKLIST: &[&str] = &[
    // semicolon, encoded slash, double slash, backslash, null, LF, CR
    ";", "%3b", "%3B", "%2f", "%2F", "//", "\\", "%5c", "%5C", "\0", "%00", "\n", "%0a", "%0A",
    "\r", "%0d", "%0D", // encoded percent and period
    "%25", "%2e", "%2E",
];

/// Decoded: U+2028 and U+2029 (plus the block list above, which their encodings already cover).
fn decoded_contains_separator(path: &str) -> bool {
    let decoded = percent_decode_lossy(path);
    decoded.contains('\u{2028}') || decoded.contains('\u{2029}')
}

fn percent_decode_lossy(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let (Some(h), Some(l)) = (
                b.get(i + 1).and_then(|c| (*c as char).to_digit(16)),
                b.get(i + 2).and_then(|c| (*c as char).to_digit(16)),
            )
        {
            out.push((h * 16 + l) as u8);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `StrictHttpFirewall#isNormalized`: no `.` or `..` path segment.
fn is_normalized(path: &str) -> bool {
    !path.split('/').any(|seg| seg == "." || seg == "..")
}

/// Whether Java's firewall (or Tomcat) rejects this raw request path (without the query).
pub fn rejects(raw_path: &str) -> bool {
    ENCODED_BLOCKLIST.iter().any(|f| raw_path.contains(f))
        || decoded_contains_separator(raw_path)
        || !is_normalized(raw_path)
        || raw_path.bytes().any(|b| !(0x20..=0x7e).contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Java answers 400 for these (measured on `/api/me/authorities/...`).
    #[test]
    fn rejected_like_java() {
        for p in [
            "/api/me/authorities/a;b=c",
            "/api/me/authorities/%2F",
            "/api/me/authorities/F%2Ejson",
            "/api/me/authorities/a%25b",
            "/api/me/authorities/a%5Cb",
            "/api/me/authorities/a%E2%80%A8b",
            "/api/me/./authorities",
            "/api/me/authorities/..",
            "/api//me/authorities",
            "/api/me/authorities/a%3Bb",
            "/api/me/authorities/a%0Ab",
            "/api/me/authorities/a%00b",
            "/api/me/authorities/a%E2%80%A9b",
        ] {
            assert!(rejects(p), "{p}");
        }
        for p in [
            "/api/me/authorities/a%20b",
            "/api/me/authorities/F.json",
            "/api/me/authorities/...",
            "/api/me/authorities/",
            "/api/me/authorities/%C3%A9",
            "/api/me/authorities/a%7Fb",
            "/api/me/authorities/a%09b",
        ] {
            assert!(!rejects(p), "{p}");
        }
    }
}
