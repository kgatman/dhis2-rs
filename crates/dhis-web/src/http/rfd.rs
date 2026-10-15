//! Spring MVC's reflected-file-download protection
//! (`AbstractMessageConverterMethodProcessor#addContentDispositionHeader`, spring-webmvc 7.0.9).
//!
//! When a body is written by a message converter (any `@ResponseBody`, including error
//! `WebMessage`s) with a status outside 1xx/3xx, and the last segment of the raw request path has
//! a file extension Spring does not consider safe, `Content-Disposition: inline;filename=f.txt`
//! is added. Safe extensions (`data/rfd-safe-extensions.txt`) are computed from the server
//! image's classes by `tools/gen-rfd-safe-extensions.sh`. Bodies written straight to the output
//! stream (e.g. `getUidCsv`) are not marked and never get the header.

use std::collections::HashSet;
use std::sync::LazyLock;

use axum::extract::Request;
use axum::middleware::Next;
use axum::response::Response;
use http::{HeaderValue, header};

use super::RequestContext;

/// Response extension: the body was written by a Spring message converter.
#[derive(Debug, Clone, Copy)]
pub struct ConverterWritten;

static SAFE: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    include_str!("../../data/rfd-safe-extensions.txt")
        .lines()
        .filter(|l| !l.is_empty())
        .collect()
});

/// `StringUtils.getFilenameExtension(path)`.
fn filename_extension(path: &str) -> Option<&str> {
    let ext = path.rfind('.')?;
    if path.rfind('/').is_some_and(|folder| folder > ext) {
        return None;
    }
    Some(&path[ext + 1..])
}

/// `safeExtension(request, ext)` for the handlers ported so far (none has a dotted pattern or
/// produces `text/html`).
fn safe_extension(ext: Option<&str>) -> bool {
    match ext {
        // StringUtils.hasText: some non-whitespace character.
        None => true,
        Some(e) if e.chars().all(char::is_whitespace) => true,
        Some(e) => SAFE.contains(e.to_lowercase().as_str()),
    }
}

/// Lossy `%XX` decoding (Spring's `StringUtils.uriDecode`; Tomcat has already rejected malformed
/// escapes in the path).
fn decode(s: &str) -> String {
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

/// Whether Spring adds the header for this raw request path.
pub fn needs_content_disposition(raw_path: &str) -> bool {
    let filename = &raw_path[raw_path.rfind('/').map_or(0, |i| i + 1)..];
    let (filename, params) = match filename.find(';') {
        Some(i) => (&filename[..i], &filename[i..]),
        None => (filename, ""),
    };
    let filename = decode(filename);
    let params = decode(params);
    !safe_extension(filename_extension(&filename)) || !safe_extension(filename_extension(&params))
}

pub async fn layer(req: Request, next: Next) -> Response {
    let raw_path = req
        .extensions()
        .get::<RequestContext>()
        .map(|c| c.original_path.clone())
        .unwrap_or_else(|| req.uri().path().to_string());
    let mut resp = next.run(req).await;
    let status = resp.status().as_u16();
    if resp.extensions().get::<ConverterWritten>().is_some()
        && !(status < 200 || (300..400).contains(&status))
        && !resp.headers().contains_key(header::CONTENT_DISPOSITION)
        && needs_content_disposition(&raw_path)
    {
        resp.headers_mut().insert(
            header::CONTENT_DISPOSITION,
            HeaderValue::from_static("inline;filename=f.txt"),
        );
    }
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Measured on Java (`/api/me/authorities/F.<ext>`).
    #[test]
    fn extensions_like_spring() {
        for ext in [
            "mp3",
            "MP3",
            "svg",
            "xhtml",
            "webp",
            "avif",
            "yml",
            "properties",
            "atom",
        ] {
            assert!(
                !needs_content_disposition(&format!("/api/me/authorities/F.{ext}")),
                "{ext}"
            );
        }
        for ext in ["wbmp", "txt", "json"] {
            assert!(
                !needs_content_disposition(&format!("/api/x/F.{ext}")),
                "{ext}"
            );
        }
        for ext in ["js", "exe", "foo", "html", "htm", "b"] {
            assert!(
                needs_content_disposition(&format!("/api/me/authorities/F.{ext}")),
                "{ext}"
            );
        }
        assert!(!needs_content_disposition("/api/me/authorities"));
        assert!(!needs_content_disposition("/api/me/authorities/F.foo/"));
        assert!(!needs_content_disposition("/api/me/settings/key."));
        // decoded before the extension is taken
        assert!(!needs_content_disposition("/api/me/authorities/F.mp%33"));
    }
}
