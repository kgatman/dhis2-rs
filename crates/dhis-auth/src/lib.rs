//! Sessions, PATs, OAuth2/OIDC, LDAP and 2FA.
//!
//! Java counterpart: dhis-web-api (org.hisp.dhis.webapi.security), dhis-service-core
//! (org.hisp.dhis.security).
//!
//! Until native auth (phase 7), Rust handlers authenticate by delegating to Java:
//! [`delegated::JavaAuthenticator`].

pub mod delegated;

use serde::Deserialize;

/// The authenticated user, as Rust handlers see it (`UserDetails` in Java).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CurrentUser {
    pub id: String,
    pub username: String,
    #[serde(default)]
    pub authorities: Vec<String>,
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
