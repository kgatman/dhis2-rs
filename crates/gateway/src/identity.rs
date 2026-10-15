//! Keeps `dhis_auth::sessions` up to date from the traffic the gateway relays (ADR 0009), and
//! resolves the identity of Rust-served requests.

use std::sync::Arc;

use dhis_auth::delegated::JavaAuthenticator;
use dhis_auth::sessions::{
    Identity, SESSION_COOKIE, SessionIdentities, basic_username, new_session_id, request_session_id,
};
use http::{HeaderMap, HeaderValue, StatusCode, header};
use tracing::debug;

use crate::routes;

/// Paths that authenticate, re-authenticate (impersonation, account creation) or end the
/// authentication of a session, possibly without changing its id.
const IDENTITY_CHANGING: &[&str] = &[
    "/api/auth/",
    "/api/account",
    "/dhis-web-commons-security/",
    "/oauth2/",
    "/login/",
];

pub struct IdentityTracker {
    store: SessionIdentities,
    db: Option<sqlx::PgPool>,
    java: JavaAuthenticator,
}

/// What the tracker needs from the request, taken before it is forwarded.
#[derive(Debug, Clone)]
pub struct Observed {
    session: Option<String>,
    basic_user: Option<String>,
    has_credentials: bool,
    identity_changing: bool,
}

impl std::fmt::Debug for IdentityTracker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IdentityTracker")
            .field("sessions", &self.store.len())
            .finish_non_exhaustive()
    }
}

impl IdentityTracker {
    pub fn new(store: SessionIdentities, db: Option<sqlx::PgPool>, java: http::Uri) -> Self {
        Self {
            store,
            db,
            java: JavaAuthenticator::new(java),
        }
    }

    pub fn observe(&self, path: &str, headers: &HeaderMap) -> Observed {
        let (normalized, _) = routes::normalize(path);
        Observed {
            session: request_session_id(headers),
            basic_user: basic_username(headers),
            has_credentials: headers.contains_key(header::AUTHORIZATION),
            identity_changing: IDENTITY_CHANGING
                .iter()
                .any(|p| normalized.starts_with(p) || normalized == p.trim_end_matches('/')),
        }
    }

    /// After Java (or Rust, relaying Java's cookies) answered: forget sessions whose
    /// authentication may have changed, and record new or re-authenticated ones.
    pub fn after_response(
        self: &Arc<Self>,
        obs: Observed,
        status: StatusCode,
        response_headers: &HeaderMap,
    ) {
        if obs.identity_changing
            && let Some(s) = &obs.session
        {
            self.store.remove(s);
        }
        let new_session = new_session_id(response_headers.get_all(header::SET_COOKIE));
        let target = match (&new_session, obs.identity_changing) {
            (Some(n), _) if Some(n) != obs.session.as_ref() => Some(n.clone()),
            (_, true) => obs.session.clone(),
            _ => None,
        };
        let Some(session) = target else { return };
        if status.as_u16() >= 400 || (!obs.identity_changing && self.store.contains(&session)) {
            return;
        }
        let this = self.clone();
        tokio::spawn(async move {
            let identity = match (
                &obs.basic_user,
                obs.session.is_some(),
                obs.identity_changing,
            ) {
                // a new session on a basic-auth request: Java built UserDetails from the DB
                (Some(user), false, false) => this.by_username(user).await,
                (None, _, _) if obs.has_credentials => None,
                _ => this.via_java(&session).await,
            };
            if let Some(i) = identity {
                debug!(uid = %i.uid, "session identity recorded");
                this.store.put(session, i);
            }
        });
    }

    async fn by_username(&self, user: &str) -> Option<Identity> {
        let db = self.db.as_ref()?;
        let (uid, group_ids) = dhis_db::users::identity_by_username(db, user)
            .await
            .ok()??;
        Some(Identity { uid, group_ids })
    }

    async fn via_java(&self, session: &str) -> Option<Identity> {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_str(&format!("{SESSION_COOKIE}={session}")).ok()?,
        );
        self.java.fetch_identity(&headers).await
    }

    /// The identity of a request Java just authenticated (`set_cookies`: what Java set while
    /// doing so). Basic auth without a session: from the database (and remembered for the new
    /// session); a known session without credentials: the record. Anything else: `None`.
    pub async fn resolve(
        &self,
        headers: &HeaderMap,
        set_cookies: &[HeaderValue],
    ) -> Option<Identity> {
        let session = request_session_id(headers);
        let credentials = headers.contains_key(header::AUTHORIZATION);
        match (session, credentials) {
            (None, true) => {
                let user = basic_username(headers)?;
                let identity = self.by_username(&user).await?;
                if let Some(new) = new_session_id(set_cookies) {
                    self.store.put(new, identity.clone());
                }
                Some(identity)
            }
            (Some(s), false) => self.store.get(&s),
            // both: Spring may keep the session's user or re-authenticate; not decided here
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn tracker() -> Arc<IdentityTracker> {
        Arc::new(IdentityTracker::new(
            SessionIdentities::new(10, Duration::from_secs(60)),
            None,
            // nothing listens here: captures from Java find nothing
            "http://127.0.0.1:9".parse().unwrap(),
        ))
    }

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut m = HeaderMap::new();
        for (k, v) in pairs {
            m.append(
                http::header::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                v.parse().unwrap(),
            );
        }
        m
    }

    fn id(uid: &str) -> Identity {
        Identity {
            uid: uid.into(),
            group_ids: vec![],
        }
    }

    #[tokio::test]
    async fn identity_changing_paths_drop_the_record() {
        let t = tracker();
        let cookie = headers(&[("cookie", "JSESSIONID=S1")]);
        for path in [
            "/api/auth/impersonate",
            "/api/auth/impersonateExit",
            "/api/41/auth/login",
            "/api/account",
            "/dhis-web-commons-security/logout.action",
        ] {
            t.store.put("S1".into(), id("U"));
            let obs = t.observe(path, &cookie);
            t.after_response(obs, StatusCode::OK, &HeaderMap::new());
            assert!(!t.store.contains("S1"), "{path}");
        }
        // other paths keep it
        t.store.put("S1".into(), id("U"));
        let obs = t.observe("/api/dataElements", &cookie);
        t.after_response(obs, StatusCode::OK, &HeaderMap::new());
        assert!(t.store.contains("S1"));
    }

    #[tokio::test]
    async fn resolve_by_kind_of_credentials() {
        let t = tracker();
        t.store.put("S1".into(), id("U"));
        assert_eq!(
            t.resolve(&headers(&[("cookie", "JSESSIONID=S1")]), &[])
                .await,
            Some(id("U"))
        );
        assert_eq!(
            t.resolve(&headers(&[("cookie", "JSESSIONID=S2")]), &[])
                .await,
            None
        );
        // a session and credentials: Spring may keep either user
        assert_eq!(
            t.resolve(
                &headers(&[("cookie", "JSESSIONID=S1"), ("authorization", "Basic YTpi")]),
                &[]
            )
            .await,
            None
        );
        // tokens: not resolved
        assert_eq!(
            t.resolve(&headers(&[("authorization", "ApiToken d2p_x")]), &[])
                .await,
            None
        );
    }
}
