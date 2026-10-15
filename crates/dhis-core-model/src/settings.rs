//! System and user settings, ported from `org.hisp.dhis.setting.LazySettings` (dhis-api).
//!
//! Definitions (key, typed default, confidential/translatable flags, property names) are
//! generated from the pinned Java build by `tools/gen-settings.sh` into `data/settings.json`.
//! Values are the raw strings stored in the `systemsetting` / `usersetting` tables; typed access
//! follows Java: parse on read, fall back to the default if the raw value does not parse.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use jiff::tz::TimeZone;
use serde::Deserialize;

use crate::java::time::{SYSTEM_TZ, parse_local_date_time};

#[derive(Debug, Clone, Deserialize)]
pub struct SettingDef {
    #[serde(rename = "type")]
    pub ty: String,
    pub default: serde_json::Value,
    #[serde(default, rename = "enumValues")]
    pub enum_values: Vec<String>,
    pub confidential: bool,
    pub translatable: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Definitions {
    system: BTreeMap<String, SettingDef>,
    user: BTreeMap<String, SettingDef>,
    key_by_property: BTreeMap<String, String>,
}

static DEFS: LazyLock<Definitions> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../data/settings.json")).expect("generated settings.json")
});

/// Definitions of all system settings with defaults (`SystemSettings.keysWithDefaults()`).
pub fn system_definitions() -> &'static BTreeMap<String, SettingDef> {
    &DEFS.system
}

pub fn user_definitions() -> &'static BTreeMap<String, SettingDef> {
    &DEFS.user
}

/// `LazySettings.isConfidential(key)`: confidential system or user setting (exact key).
pub fn is_confidential(key: &str) -> bool {
    [system_definitions(), user_definitions()]
        .iter()
        .any(|defs| defs.get(key).is_some_and(|d| d.confidential))
}

/// `LazySettings.keyOf(property)`.
pub fn key_of(property: &str) -> Option<&'static str> {
    DEFS.key_by_property.get(property).map(String::as_str)
}

/// A snapshot of stored settings (`SystemSettings.of(map)`).
#[derive(Debug, Clone, Default)]
pub struct Settings {
    /// Only non-empty values are kept, sorted by key (Java uses a sorted array + binary search).
    raw: BTreeMap<String, String>,
}

impl Settings {
    pub fn of<I, K, V>(rows: I) -> Self
    where
        I: IntoIterator<Item = (K, Option<V>)>,
        K: Into<String>,
        V: Into<String>,
    {
        let raw = rows
            .into_iter()
            .filter_map(|(k, v)| {
                let v: String = v?.into();
                (!v.is_empty()).then(|| (k.into(), v))
            })
            .collect();
        Self { raw }
    }

    /// `LazySettings#indexOf`: exact key, or for names without the `key` prefix the prefixed form
    /// (`analysisDisplayProperty` → `keyAnalysisDisplayProperty`).
    fn raw(&self, key: &str) -> Option<&str> {
        if let Some(v) = self.raw.get(key) {
            return Some(v);
        }
        if key.starts_with("key") {
            return None;
        }
        let mut chars = key.chars();
        let first = chars.next()?;
        let prefixed = format!("key{}{}", first.to_uppercase(), chars.as_str());
        self.raw.get(&prefixed).map(String::as_str)
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.raw.keys().map(String::as_str)
    }

    /// `asString(key, default)`.
    pub fn as_string(&self, key: &str, default: &str) -> String {
        self.raw(key).unwrap_or(default).to_string()
    }

    /// `asBoolean(key, default)`: `Boolean.valueOf(raw)`.
    pub fn as_bool(&self, key: &str, default: bool) -> bool {
        self.raw(key)
            .map(|r| r.eq_ignore_ascii_case("true"))
            .unwrap_or(default)
    }

    /// `asInt(key, default)`: `Integer.valueOf(raw)`, default if it does not parse.
    pub fn as_int(&self, key: &str, default: i32) -> i32 {
        self.raw(key)
            .and_then(|r| crate::java::number::integer_value_of(r).ok())
            .unwrap_or(default)
    }

    /// `asDate(key, default)` as epoch millis: digits are epoch millis, anything else
    /// `LocalDateTime.parse` in the server zone; default if unparseable.
    pub fn as_date(&self, key: &str, default_millis: i64) -> i64 {
        self.as_date_in(key, default_millis, &SYSTEM_TZ)
    }

    pub fn as_date_in(&self, key: &str, default_millis: i64, tz: &TimeZone) -> i64 {
        match self.raw(key) {
            None => default_millis,
            Some(r) if r.bytes().all(|b| b.is_ascii_digit()) => r.parse().unwrap_or(default_millis),
            Some(r) => parse_local_date_time(r, tz).unwrap_or(default_millis),
        }
    }

    /// `asEnum(key, default)`: `Enum.valueOf(raw)`, then the upper-cased raw value, else the
    /// default. `values` are the enum constant names.
    pub fn as_enum(&self, key: &str, default: &str, values: &[String]) -> String {
        match self.raw(key) {
            Some(r) if values.iter().any(|v| v == r) => r.to_string(),
            Some(r) if values.iter().any(|v| *v == r.to_uppercase()) => r.to_uppercase(),
            _ => default.to_string(),
        }
    }

    /// `asLocale(key, default)` rendered with `toString()`: `org.hisp.dhis.common.Locale.of(raw)`,
    /// the default if the raw value is not a valid locale.
    pub fn as_locale(&self, key: &str, default: &str) -> String {
        self.raw(key)
            .and_then(|r| crate::locale::Locale::parse(r).ok())
            .map_or_else(|| default.to_string(), |l| l.to_string())
    }

    /// Typed access using the generated default of a system setting.
    pub fn system_string(&self, key: &str) -> String {
        let default = system_definitions()
            .get(key)
            .and_then(|d| d.default.as_str())
            .unwrap_or("");
        self.as_string(key, default)
    }

    pub fn system_bool(&self, key: &str) -> bool {
        let default = system_definitions()
            .get(key)
            .and_then(|d| d.default.as_bool())
            .unwrap_or(false);
        self.as_bool(key, default)
    }

    pub fn system_date(&self, key: &str) -> i64 {
        let default = system_definitions()
            .get(key)
            .and_then(|d| d.default.as_i64())
            .unwrap_or(0);
        self.as_date(key, default)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn definitions_are_complete() {
        assert_eq!(system_definitions().len(), 157);
        let title = &system_definitions()["applicationTitle"];
        assert_eq!(title.default, "DHIS 2");
        assert!(title.translatable);
        assert!(system_definitions()["keyEmailPassword"].confidential);
        assert!(is_confidential("keyEmailPassword"));
        assert!(!is_confidential("keyUiLocale"));
        assert_eq!(key_of("uiLocale"), Some("keyUiLocale"));
    }

    #[test]
    fn typed_access_like_java() {
        let tz = TimeZone::get("Africa/Maseru").unwrap();
        let s = Settings::of([
            ("applicationTitle", Some("Test")),
            (
                "keyLastSuccessfulAnalyticsTablesUpdate",
                Some("2026-01-02T03:04:05.006"),
            ),
            ("keyLastMonitoringRun", Some("1700000000000")),
            ("keyVersionEnabled", Some("TRUE")),
            ("keyCacheStrategy", Some("cache_6am_tomorrow")),
            ("keyEmpty", Some("")),
            ("keyBadDate", Some("not a date")),
        ]);
        assert_eq!(s.system_string("applicationTitle"), "Test");
        assert_eq!(s.system_string("keyApplicationFooter"), "");
        assert!(s.system_bool("keyVersionEnabled"));
        assert_eq!(
            crate::java::time::format_iso_millis_in(
                s.as_date_in("keyLastSuccessfulAnalyticsTablesUpdate", 0, &tz),
                &tz
            ),
            "2026-01-02T03:04:05.006"
        );
        assert_eq!(s.as_date("keyLastMonitoringRun", 0), 1_700_000_000_000);
        assert_eq!(s.as_date("keyBadDate", 7), 7);
        // Name without "key" prefix resolves to the prefixed key.
        assert_eq!(s.as_string("versionEnabled", ""), "TRUE");
        let values = vec!["CACHE_6AM_TOMORROW".to_string(), "NO_CACHE".to_string()];
        assert_eq!(
            s.as_enum("keyCacheStrategy", "NO_CACHE", &values),
            "CACHE_6AM_TOMORROW"
        );
        assert_eq!(s.keys().count(), 6, "empty values are dropped");
    }
}
