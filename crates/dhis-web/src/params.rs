//! Request parameter binding with Spring MVC semantics.
//!
//! Tomcat decodes the query string as `application/x-www-form-urlencoded` (UTF-8, `+` = space).
//! A single-valued parameter bound from several occurrences takes the first one (measured:
//! `limit=3&limit=5` → 3). Names are case-sensitive.

use axum::extract::FromRequestParts;
use axum::response::Response;

use crate::http::web_message::WebMessage;
use crate::java::number::spring_integer;

#[derive(Debug, Clone, Default)]
pub struct Params(Vec<(String, String)>);

impl Params {
    pub fn parse(query: Option<&str>) -> Self {
        Self(
            query
                .map(|q| {
                    form_urlencoded::parse(q.as_bytes())
                        .map(|(k, v)| (k.into_owned(), v.into_owned()))
                        .collect()
                })
                .unwrap_or_default(),
        )
    }

    pub fn first(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// All values of `name`, in request order (`getParameterValues`).
    pub fn all(&self, name: &str) -> Vec<&str> {
        self.0
            .iter()
            .filter(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
            .collect()
    }

    /// `@RequestParam(required = false) Set<String> name`: `None` when absent. A single value is
    /// split at commas and each part trimmed (`StringToCollectionConverter`); several values are
    /// taken as they are (`ArrayToCollectionConverter`). Duplicates keep their first position
    /// (`LinkedHashSet`).
    pub fn string_set(&self, name: &str) -> Option<Vec<String>> {
        let values = self.all(name);
        let items: Vec<String> = match values.as_slice() {
            [] => return None,
            [""] => vec![],
            [single] => single
                .split(',')
                .map(|p| crate::java::string::trim(p).to_string())
                .collect(),
            many => many.iter().map(|v| v.to_string()).collect(),
        };
        let mut out: Vec<String> = Vec::with_capacity(items.len());
        for i in items {
            if !out.contains(&i) {
                out.push(i);
            }
        }
        Some(out)
    }

    /// `@RequestParam(required = false, defaultValue = ...) Integer name`, with the
    /// `CrudControllerAdvice#methodArgumentTypeMismatchException` message on failure.
    pub fn integer_or(&self, name: &str, default: i32) -> Result<i32, WebMessage> {
        let Some(raw) = self.first(name) else {
            return Ok(default);
        };
        match spring_integer(raw) {
            Ok(Some(v)) => Ok(v),
            Ok(None) => Ok(default),
            Err(e) => Err(WebMessage::bad_request(format!(
                "Value '{raw}' is not valid for parameter {name}. {}",
                e.0
            ))),
        }
    }
}

impl<S: Send + Sync> FromRequestParts<S> for Params {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut http::request::Parts,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        Ok(Params::parse(parts.uri.query()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_value_and_decoding() {
        let p = Params::parse(Some("limit=3&limit=5&a=x+y&b=%E2%82%AC"));
        assert_eq!(p.first("limit"), Some("3"));
        assert_eq!(p.first("a"), Some("x y"));
        assert_eq!(p.first("b"), Some("€"));
        assert_eq!(p.first("LIMIT"), None);
    }

    #[test]
    fn integer_errors_match_java() {
        let p = Params::parse(Some("limit=abc"));
        assert_eq!(
            p.integer_or("limit", 1).unwrap_err().message.as_deref(),
            Some("Value 'abc' is not valid for parameter limit. For input string: \"abc\"")
        );
        assert_eq!(Params::parse(Some("limit=")).integer_or("limit", 1), Ok(1));
        assert_eq!(
            Params::parse(Some("limit=0x10")).integer_or("limit", 1),
            Ok(16)
        );
    }
}
