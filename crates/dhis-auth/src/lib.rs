//! Sessions, PATs, OAuth2/OIDC, LDAP and 2FA.
//!
//! Java counterpart: dhis-web-api (org.hisp.dhis.webapi.security), dhis-service-core
//! (org.hisp.dhis.security).
//!
//! Until native auth (phase 7), Rust handlers authenticate by delegating to Java:
//! [`delegated::JavaAuthenticator`].

pub mod delegated;
pub mod sessions;

use serde::de::{MapAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::value::RawValue;

/// The authenticated user, as Rust handlers see it (`UserDetails` in Java). Delegated
/// authentication asks Java for one of these per request ([`delegated::Probe`]); the other is
/// `None`, and a handler needing it falls back to Java.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CurrentUser {
    /// `UserDetails#getAllAuthorities()` of the session, in Java's iteration order
    /// (`/api/me/authorities`).
    pub authorities: Option<Vec<String>>,
    /// `UserSettings.getCurrentSettings().toJson(false)` as Java rendered it for this request
    /// (`/api/me/settings`). Java keeps user settings per session in memory
    /// (`SessionUserSettings`, refreshed on authentication and on user-setting writes, not on
    /// system-setting writes), so they cannot be recomputed from the database (ADR 0006).
    pub settings: Option<JsonMembers>,
    /// The user id and group ids of the session's `UserDetails`, for sharing checks; `None`
    /// unless the route needs them and they are known (`sessions`, ADR 0009).
    pub identity: Option<sessions::Identity>,
}

/// The members of a JSON object in document order, each value as the exact JSON text Java wrote.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JsonMembers(pub Vec<(String, String)>);

impl JsonMembers {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(|(k, _)| k.as_str())
    }
}

impl<'de> Deserialize<'de> for JsonMembers {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = JsonMembers;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a JSON object")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<JsonMembers, A::Error> {
                let mut out = Vec::new();
                while let Some((k, v)) = map.next_entry::<String, Box<RawValue>>()? {
                    out.push((k, v.get().to_string()));
                }
                Ok(JsonMembers(out))
            }
        }
        d.deserialize_map(V)
    }
}

impl CurrentUser {
    /// `UserDetails#isSuper()`: has the `ALL` authority (`None` without authorities).
    pub fn is_super(&self) -> Option<bool> {
        Some(self.authorities.as_ref()?.iter().any(|a| a == "ALL"))
    }

    /// `UserDetails#isAuthorized(String)`: super users have every authority.
    pub fn is_authorized(&self, authority: &str) -> Option<bool> {
        let auths = self.authorities.as_ref()?;
        Some(auths.iter().any(|a| a == "ALL" || a == authority))
    }
}
