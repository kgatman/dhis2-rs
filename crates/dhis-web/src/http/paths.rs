//! `org.hisp.dhis.webapi.utils.HttpServletRequestPaths`: absolute URLs as Java builds them from
//! the request (Host and `X-Forwarded-*` headers).

use http::HeaderMap;

/// `HttpServletRequestPaths#getContextPath` behind no servlet context path (DHIS2's default).
pub fn context_path(headers: &HeaderMap) -> String {
    let get = |n: &str| headers.get(n).and_then(|v| v.to_str().ok());
    let mut out = String::new();
    match get("x-forwarded-proto") {
        Some(p) if p.eq_ignore_ascii_case("http") || p.eq_ignore_ascii_case("https") => {
            out.push_str(p)
        }
        // Tomcat's own connector scheme; TLS terminates before Java/the gateway.
        _ => out.push_str("http"),
    }
    out.push_str("://");
    let host_header = get("host").unwrap_or("");
    // Tomcat: server name/port from the Host header (IPv6 literals keep their brackets).
    let (server_name, server_port) = match host_header.rsplit_once(':') {
        Some((h, p)) if !h.ends_with(']') || host_header.starts_with('[') => match p.parse() {
            Ok(port) => (h.to_string(), port),
            Err(_) => (host_header.to_string(), 80),
        },
        _ => (host_header.to_string(), 80),
    };
    match get("x-forwarded-host").filter(|h| !h.trim().is_empty()) {
        Some(h) => {
            let host = h.split(',').next().unwrap_or("").trim();
            out.push_str(host.split(':').next().unwrap_or(host));
        }
        None => out.push_str(&server_name),
    }
    let port = get("x-forwarded-port")
        .and_then(|p| dhis_core_model::java::number::integer_value_of(p).ok())
        .unwrap_or(server_port);
    if port != 80 && port != 443 {
        out.push(':');
        out.push_str(&port.to_string());
    }
    out
}

/// `ApiVersionFilter`: the request URI controllers see. `/api/{28..44}/rest` becomes `/api/rest`
/// (the filter wraps the request and overrides `getRequestURI`); anything else is unchanged.
pub fn versionless_request_uri(raw: &str) -> String {
    if let Some(rest) = raw.strip_prefix("/api/") {
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 2
            && rest.as_bytes().get(2) == Some(&b'/')
            && (28..=44).contains(&rest[..2].parse::<u32>().unwrap_or(0))
        {
            return format!("/api/{}", &rest[3..]);
        }
    }
    raw.to_string()
}

/// `HttpServletRequestPaths#getApiPath`: the context path, plus `/api` and the version found by
/// `(/api/(\\d+)?/)` in the request URI. Controllers see the URI after `ApiVersionFilter`, so
/// `/api/41/schemas` gives no version; only a later `/api/<n>/` in the path (or a version outside
/// 28..44) does. An empty version (`/api//`) gives Java's `/api/null`.
pub fn api_path(headers: &HeaderMap, raw_request_uri: &str) -> String {
    let request_uri = versionless_request_uri(raw_request_uri);
    let request_uri = request_uri.as_str();
    let mut out = context_path(headers);
    out.push_str("/api");
    let b = request_uri.as_bytes();
    let mut from = 0;
    while let Some(i) = request_uri[from..].find("/api/") {
        let start = from + i + 5;
        let digits = b[start..].iter().take_while(|c| c.is_ascii_digit()).count();
        if b.get(start + digits) == Some(&b'/') {
            out.push('/');
            if digits == 0 {
                out.push_str("null");
            } else {
                out.push_str(&request_uri[start..start + digits]);
            }
            break;
        }
        from = from + i + 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut m = HeaderMap::new();
        for (k, v) in pairs {
            m.insert(
                http::header::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                v.parse().unwrap(),
            );
        }
        m
    }

    #[test]
    fn context_path_like_java() {
        assert_eq!(
            context_path(&h(&[("host", "localhost:8081")])),
            "http://localhost:8081"
        );
        assert_eq!(context_path(&h(&[("host", "web")])), "http://web");
        assert_eq!(context_path(&h(&[("host", "web:80")])), "http://web");
        assert_eq!(
            context_path(&h(&[
                ("host", "web:8080"),
                ("x-forwarded-proto", "https"),
                ("x-forwarded-host", "play.dhis2.org:9999, x"),
                ("x-forwarded-port", "443"),
            ])),
            "https://play.dhis2.org"
        );
        assert_eq!(
            context_path(&h(&[("host", "web:8080"), ("x-forwarded-proto", "ftp")])),
            "http://web:8080"
        );
    }

    #[test]
    fn api_path_like_java() {
        let host = h(&[("host", "localhost:8081")]);
        assert_eq!(api_path(&host, "/api/schemas"), "http://localhost:8081/api");
        // ApiVersionFilter strips 28..44 first (measured: /api/41/schemas/user.json links to /api)
        assert_eq!(
            api_path(&host, "/api/41/schemas.json"),
            "http://localhost:8081/api"
        );
        assert_eq!(
            api_path(&host, "/api/45/schemas"),
            "http://localhost:8081/api/45"
        );
        assert_eq!(
            api_path(&host, "/api/41x/a/api/40/b"),
            "http://localhost:8081/api/40"
        );
        assert_eq!(versionless_request_uri("/api/28/x"), "/api/x");
        assert_eq!(versionless_request_uri("/api/27/x"), "/api/27/x");
        assert_eq!(versionless_request_uri("/api/4/x"), "/api/4/x");
        assert_eq!(
            api_path(&host, "/api/schemas/api/7/"),
            "http://localhost:8081/api/7"
        );
    }
}
