//! Sessions, PATs, OAuth2/OIDC, LDAP and 2FA.
//!
//! Java counterpart: dhis-web-api (org.hisp.dhis.webapi.security), dhis-service-core
//! (org.hisp.dhis.security).
//!
//! Until native auth (phase 7), Rust handlers authenticate by delegating to Java:
//! [`delegated::JavaAuthenticator`].

pub mod delegated;

use serde::de::{MapAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::value::RawValue;

/// The authenticated user, as Rust handlers see it (`UserDetails` in Java).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CurrentUser {
    pub id: String,
    pub username: String,
    /// In the order Java listed them (`User#getAllAuthorities()`, an immutable set whose
    /// iteration order is randomised per JVM start).
    #[serde(default)]
    pub authorities: Vec<String>,
    /// `UserSettings.getCurrentSettings().toJson(false)` as Java rendered it for this request.
    /// Java keeps user settings per session in memory (`SessionUserSettings`, refreshed on
    /// authentication and on user-setting writes, not on system-setting writes), so they cannot
    /// be recomputed from the database. `None` when Java did not send them.
    #[serde(default)]
    pub settings: Option<JsonMembers>,
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
    /// `UserDetails#isSuper()`: has the `ALL` authority.
    pub fn is_super(&self) -> bool {
        self.authorities.iter().any(|a| a == "ALL")
    }

    /// `UserDetails#isAuthorized(String)`: super users have every authority.
    pub fn is_authorized(&self, authority: &str) -> bool {
        self.is_super() || self.authorities.iter().any(|a| a == authority)
    }
}
