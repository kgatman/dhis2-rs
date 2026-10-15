//! Content negotiation, deliberately conservative.
//!
//! Spring MVC picks the handler by matching `produces` against the requested media types (path
//! extension first, via DHIS2's suffix strategy, then `Accept`). Only unambiguous cases are decided
//! here; anything else returns `None` and the handler falls back to Java, which then produces the
//! exact response (including Tomcat's HTML 406 pages).

use http::HeaderMap;

use super::RequestContext;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Representation {
    Json,
    /// `application/csv` (the `produces` of the CSV variants of several endpoints).
    ApplicationCsv,
}

#[derive(Debug, Clone, PartialEq)]
struct Accepted {
    ty: String,
    sub: String,
}

impl Accepted {
    fn specificity(&self) -> u8 {
        match (self.ty.as_str(), self.sub.as_str()) {
            ("*", "*") => 0,
            (_, "*") => 1,
            _ => 2,
        }
    }
}

/// Parses `Accept`; `None` if it uses parameters (q-values etc.) we do not model.
fn parse_accept(headers: &HeaderMap) -> Option<Vec<Accepted>> {
    let mut out = Vec::new();
    for value in headers.get_all(http::header::ACCEPT) {
        let value = value.to_str().ok()?;
        for part in value.split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            if part.contains(';') {
                return None;
            }
            let (ty, sub) = part.split_once('/')?;
            out.push(Accepted {
                ty: ty.trim().to_ascii_lowercase(),
                sub: sub.trim().to_ascii_lowercase(),
            });
        }
    }
    if out.is_empty() {
        out.push(Accepted {
            ty: "*".into(),
            sub: "*".into(),
        });
    }
    // Spring sorts by specificity (stable), most specific first.
    out.sort_by_key(|a| std::cmp::Reverse(a.specificity()));
    Some(out)
}

fn matches(a: &Accepted, rep: Representation) -> bool {
    let (ty, sub) = match rep {
        Representation::Json => ("application", "json"),
        Representation::ApplicationCsv => ("application", "csv"),
    };
    (a.ty == "*" || a.ty == ty) && (a.sub == "*" || a.sub == sub)
}

/// Picks one of `offered` (in handler-preference order; the first wins on wildcards), or
/// `None` when the case is not modelled and Java must decide.
pub fn choose(
    headers: &HeaderMap,
    ctx: Option<&RequestContext>,
    offered: &[Representation],
) -> Option<Representation> {
    if let Some(ext) = ctx.and_then(|c| c.extension.as_deref()) {
        // The suffix forces the media type.
        return match ext {
            "json" if offered.contains(&Representation::Json) => Some(Representation::Json),
            _ => None,
        };
    }
    let accepted = parse_accept(headers)?;
    let first = accepted.first()?;
    // Only decide when the most specific accepted type picks a representation unambiguously.
    if first.specificity() < 2 {
        return offered.first().copied();
    }
    offered.iter().copied().find(|r| matches(first, *r))
}

#[cfg(test)]
mod tests {
    use super::*;
    use Representation::*;

    fn h(accept: Option<&str>) -> HeaderMap {
        let mut m = HeaderMap::new();
        if let Some(a) = accept {
            m.insert(http::header::ACCEPT, a.parse().unwrap());
        }
        m
    }

    const BOTH: &[Representation] = &[Json, ApplicationCsv];

    /// Measured against Java on `/api/system/uid`.
    #[test]
    fn measured_cases() {
        assert_eq!(choose(&h(None), None, BOTH), Some(Json));
        assert_eq!(choose(&h(Some("*/*")), None, BOTH), Some(Json));
        assert_eq!(choose(&h(Some("application/json")), None, BOTH), Some(Json));
        assert_eq!(
            choose(&h(Some("application/csv")), None, BOTH),
            Some(ApplicationCsv)
        );
        assert_eq!(
            choose(&h(Some("application/csv, application/json")), None, BOTH),
            Some(ApplicationCsv)
        );
        // Java answers 406 (Tomcat HTML) for these; we let Java do it.
        assert_eq!(choose(&h(Some("text/csv")), None, BOTH), None);
        assert_eq!(choose(&h(Some("application/xml")), None, BOTH), None);
        let json = RequestContext {
            original_path: "/api/system/uid.json".into(),
            extension: Some("json".into()),
        };
        assert_eq!(choose(&h(None), Some(&json), BOTH), Some(Json));
        let csv = RequestContext {
            original_path: "/api/system/uid.csv".into(),
            extension: Some("csv".into()),
        };
        assert_eq!(choose(&h(None), Some(&csv), BOTH), None);
    }

    #[test]
    fn unmodelled_accept_falls_back() {
        assert_eq!(choose(&h(Some("application/json;q=0.5")), None, BOTH), None);
    }
}
