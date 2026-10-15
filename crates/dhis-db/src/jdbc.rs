//! Parses `jdbc:postgresql:` URLs the way pgjdbc (`org.postgresql.Driver#parseURL`) does, for the
//! subset DHIS2 deployments use: one host, optional port and database, query parameters.
//!
//! - `jdbc:postgresql:db`, `jdbc:postgresql:/db`, `jdbc:postgresql://host`,
//!   `jdbc:postgresql://host:port/db?k=v&...` and IPv6 `[::1]` hosts;
//! - the default host is `localhost`, the default port 5432, the default database is the user;
//! - URL parameters override the user/password passed by the pool (pgjdbc puts URL params on top
//!   of the driver properties);
//! - multi-host URLs (failover) are rejected for now.

use std::collections::BTreeMap;

use sqlx::postgres::{PgConnectOptions, PgSslMode};
use thiserror::Error;
use tracing::warn;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum JdbcUrlError {
    #[error("not a jdbc:postgresql: URL: {0}")]
    NotPostgres(String),
    #[error("bad port in JDBC URL: {0}")]
    BadPort(String),
    #[error("multi-host JDBC URLs are not supported yet: {0}")]
    MultiHost(String),
    #[error("unsupported sslmode: {0}")]
    BadSslMode(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JdbcUrl {
    pub host: String,
    pub port: u16,
    pub database: Option<String>,
    pub params: BTreeMap<String, String>,
}

fn decode(s: &str) -> String {
    // URLDecoder.decode(s, "UTF-8") as pgjdbc does for each component.
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => match u8::from_str_radix(&s[i + 1..i + 3], 16) {
                Ok(b) => {
                    out.push(b);
                    i += 2;
                }
                Err(_) => out.push(b'%'),
            },
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

impl JdbcUrl {
    pub fn parse(url: &str) -> Result<Self, JdbcUrlError> {
        let rest = url
            .strip_prefix("jdbc:postgresql:")
            .ok_or_else(|| JdbcUrlError::NotPostgres(url.to_string()))?;
        let (main, query) = match rest.split_once('?') {
            Some((m, q)) => (m, Some(q)),
            None => (rest, None),
        };

        let mut params = BTreeMap::new();
        if let Some(q) = query {
            for pair in q.split('&').filter(|p| !p.is_empty()) {
                let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
                params.insert(k.to_string(), decode(v));
            }
        }

        let (host, port, database) = if let Some(after) = main.strip_prefix("//") {
            let (hostport, db) = match after.split_once('/') {
                Some((h, d)) => (h, Some(d)),
                None => (after, None),
            };
            if hostport.contains(',') {
                return Err(JdbcUrlError::MultiHost(url.to_string()));
            }
            let (host, port) = if let Some(v6) = hostport.strip_prefix('[') {
                let (h, tail) = v6.split_once(']').unwrap_or((v6, ""));
                (h.to_string(), tail.strip_prefix(':'))
            } else {
                match hostport.rsplit_once(':') {
                    Some((h, p)) => (h.to_string(), Some(p)),
                    None => (hostport.to_string(), None),
                }
            };
            let port = match port.filter(|p| !p.is_empty()) {
                Some(p) => p
                    .parse()
                    .map_err(|_| JdbcUrlError::BadPort(p.to_string()))?,
                None => 5432,
            };
            let host = if host.is_empty() {
                "localhost".into()
            } else {
                host
            };
            (host, port, db.map(decode))
        } else {
            let db = main.strip_prefix('/').unwrap_or(main);
            ("localhost".to_string(), 5432, Some(decode(db)))
        };

        Ok(Self {
            host,
            port,
            database: database.filter(|d| !d.is_empty()),
            params,
        })
    }

    /// Builds sqlx options; `user`/`password` come from `dhis.conf` unless the URL sets them.
    pub fn connect_options(&self, user: Option<&str>, password: Option<&str>) -> PgConnectOptions {
        let user = self.params.get("user").map(String::as_str).or(user);
        let password = self.params.get("password").map(String::as_str).or(password);

        let mut o = PgConnectOptions::new_without_pgpass()
            .host(&self.host)
            .port(self.port);
        if let Some(u) = user {
            o = o.username(u);
        }
        if let Some(p) = password {
            o = o.password(p);
        }
        // pgjdbc: database defaults to the user name.
        if let Some(db) = self.database.as_deref().or(user) {
            o = o.database(db);
        }

        for (k, v) in &self.params {
            match k.as_str() {
                "user" | "password" => {}
                "sslmode" => match ssl_mode(v) {
                    Ok(m) => o = o.ssl_mode(m),
                    Err(e) => warn!("{e}; keeping the default"),
                },
                // pgjdbc: ssl=true without sslmode means verify-full.
                "ssl" if v == "true" && !self.params.contains_key("sslmode") => {
                    o = o.ssl_mode(PgSslMode::VerifyFull)
                }
                "ssl" => {}
                "sslrootcert" => o = o.ssl_root_cert(v),
                "sslcert" => o = o.ssl_client_cert(v),
                "sslkey" => o = o.ssl_client_key(v),
                "ApplicationName" | "applicationName" => o = o.application_name(v),
                "currentSchema" => o = o.options([("search_path", v.as_str())]),
                "options" => o = o.options(parse_options(v)),
                other => {
                    warn!("JDBC URL parameter {other} is not supported by the Rust pool; ignored")
                }
            }
        }
        o
    }
}

fn ssl_mode(v: &str) -> Result<PgSslMode, JdbcUrlError> {
    Ok(match v {
        "disable" => PgSslMode::Disable,
        "allow" => PgSslMode::Allow,
        "prefer" => PgSslMode::Prefer,
        "require" => PgSslMode::Require,
        "verify-ca" => PgSslMode::VerifyCa,
        "verify-full" => PgSslMode::VerifyFull,
        other => return Err(JdbcUrlError::BadSslMode(other.to_string())),
    })
}

/// `options=-c key=value -c key2=value2`.
fn parse_options(v: &str) -> Vec<(String, String)> {
    v.split("-c")
        .filter_map(|p| {
            let (k, val) = p.trim().split_once('=')?;
            Some((k.trim().to_string(), val.trim().to_string()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> JdbcUrl {
        JdbcUrl::parse(s).unwrap()
    }

    #[test]
    fn forms() {
        let u = p("jdbc:postgresql://db/dhis");
        assert_eq!(
            (u.host.as_str(), u.port, u.database.as_deref()),
            ("db", 5432, Some("dhis"))
        );
        let u = p("jdbc:postgresql://db:5433/dhis?sslmode=require&ApplicationName=x%20y");
        assert_eq!(u.port, 5433);
        assert_eq!(u.params["sslmode"], "require");
        assert_eq!(u.params["ApplicationName"], "x y");
        let u = p("jdbc:postgresql:dhis");
        assert_eq!(
            (u.host.as_str(), u.database.as_deref()),
            ("localhost", Some("dhis"))
        );
        let u = p("jdbc:postgresql://[::1]:6000/x");
        assert_eq!((u.host.as_str(), u.port), ("::1", 6000));
        let u = p("jdbc:postgresql://h");
        assert_eq!(u.database, None);
    }

    #[test]
    fn errors() {
        assert!(matches!(
            JdbcUrl::parse("jdbc:mysql://x/y"),
            Err(JdbcUrlError::NotPostgres(_))
        ));
        assert!(matches!(
            JdbcUrl::parse("jdbc:postgresql://a,b/y"),
            Err(JdbcUrlError::MultiHost(_))
        ));
        assert!(matches!(
            JdbcUrl::parse("jdbc:postgresql://a:xx/y"),
            Err(JdbcUrlError::BadPort(_))
        ));
    }

    #[test]
    fn url_credentials_win() {
        let o = p("jdbc:postgresql://h/db?user=u2").connect_options(Some("u1"), Some("pw"));
        assert_eq!(o.get_username(), "u2");
        let o = p("jdbc:postgresql://h").connect_options(Some("dhis"), None);
        assert_eq!(o.get_database(), Some("dhis"));
    }
}
