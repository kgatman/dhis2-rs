//! The identity behind a Java session, as far as sharing checks need it (ADR 0009).
//!
//! Java keeps the authenticated `UserDetails` in its own in-memory session; the user id and
//! group ids in it are fixed when the session is authenticated (measured: removing a user from a
//! group does not change what an existing session can read; a new session reflects it).
//! Rust cannot read that memory, so the gateway records the same values when it sees Java
//! create or re-authenticate a session, keyed by the session id (`JSESSIONID`):
//!
//! - a new session on a basic-auth request: the user is looked up in the database, as Java
//!   builds `UserDetails` from it for that request;
//! - any other new session (form or OAuth2 login, ...): `/api/me?fields=id,userGroups[id]` on
//!   the new session right away;
//! - impersonation, exiting it and logout change or end the authentication of a session
//!   without a new id: the record is dropped (and taken again after impersonation).
//!
//! Every request is still authenticated by Java (ADR 0004): a record is only used once Java has
//! accepted the session for that very request. Unknown sessions go to Java.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use base64::Engine;
use http::{HeaderMap, HeaderValue, header};

/// `UserDetails#getUid()` and `#getUserGroupIds()` of a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub uid: String,
    /// Sorted (Java sorts them when it builds the sharing filter).
    pub group_ids: Vec<String>,
}

/// Tomcat's session cookie name (DHIS2 keeps the default; measured).
pub const SESSION_COOKIE: &str = "JSESSIONID";

struct Entry {
    identity: Identity,
    last_used: Instant,
}

/// Session id → identity, bounded in size, entries dropped once idle for longer than Java's
/// session timeout (the session is gone in Java by then).
pub struct SessionIdentities {
    map: Mutex<HashMap<String, Entry>>,
    capacity: usize,
    idle: Duration,
}

impl SessionIdentities {
    pub fn new(capacity: usize, idle: Duration) -> Self {
        Self {
            map: Mutex::new(HashMap::new()),
            capacity,
            idle,
        }
    }

    pub fn get(&self, session_id: &str) -> Option<Identity> {
        let mut map = self.map.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        let e = map.get_mut(session_id)?;
        if now.duration_since(e.last_used) > self.idle {
            map.remove(session_id);
            return None;
        }
        e.last_used = now;
        Some(e.identity.clone())
    }

    pub fn contains(&self, session_id: &str) -> bool {
        self.map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(session_id)
    }

    pub fn put(&self, session_id: String, identity: Identity) {
        let mut map = self.map.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        if map.len() >= self.capacity && !map.contains_key(&session_id) {
            let idle = self.idle;
            map.retain(|_, e| now.duration_since(e.last_used) <= idle);
            if map.len() >= self.capacity {
                // still full: drop the least recently used
                if let Some(oldest) = map
                    .iter()
                    .min_by_key(|(_, e)| e.last_used)
                    .map(|(k, _)| k.clone())
                {
                    map.remove(&oldest);
                }
            }
        }
        map.insert(
            session_id,
            Entry {
                identity,
                last_used: now,
            },
        );
    }

    pub fn remove(&self, session_id: &str) {
        self.map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(session_id);
    }

    pub fn len(&self) -> usize {
        self.map.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The value of the session cookie the client sent. Tomcat uses the first `JSESSIONID` that names
/// a valid session; with several cookies of that name the choice depends on Java's state, so
/// more than one gives `None`.
pub fn request_session_id(headers: &HeaderMap) -> Option<String> {
    let mut found: Option<String> = None;
    for v in headers.get_all(header::COOKIE) {
        let Ok(v) = v.to_str() else { continue };
        for part in v.split(';') {
            if let Some((n, val)) = part.split_once('=')
                && n.trim() == SESSION_COOKIE
            {
                if found.is_some() {
                    return None;
                }
                found = Some(val.trim().to_string());
            }
        }
    }
    found
}

/// The session id in Java's `Set-Cookie` headers, if it set one.
pub fn new_session_id<'a>(
    set_cookies: impl IntoIterator<Item = &'a HeaderValue>,
) -> Option<String> {
    set_cookies
        .into_iter()
        .filter_map(|v| v.to_str().ok())
        .filter_map(|v| v.split(';').next())
        .filter_map(|nv| nv.split_once('='))
        .find(|(n, _)| n.trim() == SESSION_COOKIE)
        .map(|(_, v)| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// The username of a basic-auth `Authorization` header, as Spring's
/// `BasicAuthenticationConverter` reads it (scheme case-insensitive, standard Base64, UTF-8,
/// split at the first `:`). `None` for other schemes or tokens Spring rejects.
pub fn basic_username(headers: &HeaderMap) -> Option<String> {
    let mut values = headers.get_all(header::AUTHORIZATION).iter();
    let header = values.next()?.to_str().ok()?;
    if values.next().is_some() {
        return None;
    }
    let header = header.trim();
    let bytes = header.as_bytes();
    if bytes.len() <= 6 || !bytes[..6].eq_ignore_ascii_case(b"basic ") {
        return None;
    }
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(&bytes[6..])
        .ok()?;
    // Java decodes invalid UTF-8 with replacement characters; such a name matches no user.
    let token = String::from_utf8(decoded).ok()?;
    let (user, _) = token.split_once(':')?;
    Some(user.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut m = HeaderMap::new();
        for (k, v) in pairs {
            m.append(
                http::header::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                v.parse().unwrap(),
            );
        }
        m
    }

    #[test]
    fn cookies_and_credentials() {
        assert_eq!(
            request_session_id(&h(&[("cookie", "a=1; JSESSIONID=ABC")])).as_deref(),
            Some("ABC")
        );
        assert_eq!(
            request_session_id(&h(&[
                ("cookie", "JSESSIONID=A"),
                ("cookie", "JSESSIONID=B")
            ])),
            None
        );
        let set = [HeaderValue::from_static("JSESSIONID=NEW; Path=/; HttpOnly")];
        assert_eq!(new_session_id(&set).as_deref(), Some("NEW"));
        assert_eq!(
            basic_username(&h(&[("authorization", "basic YWRtaW46ZGlzdHJpY3Q=")])).as_deref(),
            Some("admin")
        );
        assert_eq!(
            basic_username(&h(&[("authorization", "Basic dTpwOnE=")])).as_deref(),
            Some("u")
        );
        assert_eq!(basic_username(&h(&[("authorization", "Bearer x")])), None);
        assert_eq!(basic_username(&h(&[("authorization", "Basic !!")])), None);
    }

    #[test]
    fn store_expiry_and_capacity() {
        let id = |u: &str| Identity {
            uid: u.into(),
            group_ids: vec![],
        };
        let s = SessionIdentities::new(2, Duration::from_secs(60));
        s.put("a".into(), id("A"));
        s.put("b".into(), id("B"));
        s.get("a");
        s.put("c".into(), id("C"));
        assert_eq!(s.len(), 2);
        assert!(s.contains("a") && s.contains("c") && !s.contains("b"));
        let idle = SessionIdentities::new(2, Duration::ZERO);
        idle.put("a".into(), id("A"));
        std::thread::sleep(Duration::from_millis(2));
        assert_eq!(idle.get("a"), None);
    }
}
