//! Ported from `org.hisp.dhis.common.Locale` (dhis-api): DHIS2's own locale value, used for
//! locale-typed settings. Stricter than `java.util.Locale`: the language must be 2–3 lower-case
//! ASCII letters, the region 2 upper-case letters or 3 digits, the script `Xxxx`.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Locale {
    pub language: String,
    pub region: Option<String>,
    pub script: Option<String>,
}

impl Locale {
    /// `Locale.of(String)`: BCP47 (`fr-CA`) or Java format (`fr_CA`, `zh_TW_Hant`, `zh_TW_#Hant`).
    /// A third part that is not a script is "misunderstood" as the script, as in Java.
    pub fn parse(locale: &str) -> Result<Self, String> {
        // Java checks lengths in UTF-16 units; any non-ASCII input fails validation either way.
        let len = locale.encode_utf16().count();
        if len == 2 || len == 3 {
            return Self::new(locale, None, None);
        }
        let b = locale.as_bytes();
        if len == 5 && locale.is_ascii() && (b[2] == b'-' || b[2] == b'_') {
            return Self::new(&locale[0..2], Some(&locale[3..5]), None);
        }
        let parts = java_split(locale);
        match parts.as_slice() {
            [language, region] => Self::new(language, Some(region), None),
            [language, second, third] if is_script(Some(second)) => {
                Self::new(language, Some(third), Some(second))
            }
            [language, region, script] => Self::new(language, Some(region), Some(script)),
            _ => Err(format!("Invalid locale: {locale}")),
        }
    }

    /// The canonical constructor: validates and maps the outdated codes `iw`, `ji`, `in`.
    fn new(language: &str, region: Option<&str>, script: Option<&str>) -> Result<Self, String> {
        let lower = |c: u8| c.is_ascii_lowercase();
        if !(2..=3).contains(&language.len()) || !language.bytes().all(lower) {
            return Err(format!("Invalid language code: {language}"));
        }
        if let Some(r) = region {
            let ok = (r.len() == 2 && r.bytes().all(|c| c.is_ascii_uppercase()))
                || (r.len() == 3 && r.bytes().all(|c| c.is_ascii_digit()));
            if !ok {
                return Err(format!("Invalid region code: {r}"));
            }
        }
        if !is_script(script) {
            return Err(format!("Invalid script: {}", script.unwrap_or_default()));
        }
        if script.is_some() && region.is_none() {
            return Err("Script must be used with region".into());
        }
        let language = match language {
            "iw" => "he",
            "ji" => "yi",
            "in" => "id",
            other => other,
        };
        Ok(Self {
            language: language.into(),
            region: region.map(Into::into),
            script: script.map(Into::into),
        })
    }
}

fn is_script(s: Option<&str>) -> bool {
    match s {
        None => true,
        Some(s) => {
            let b = s.as_bytes();
            b.len() == 4 && b[0].is_ascii_uppercase() && b[1..].iter().all(u8::is_ascii_lowercase)
        }
    }
}

/// `locale.split("-|_#|_")`: alternatives tried in that order at each position; trailing empty
/// strings removed.
fn java_split(s: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut i = 0;
    let b = s.as_bytes();
    while i < b.len() {
        let sep = match b[i] {
            b'-' => 1,
            b'_' if b.get(i + 1) == Some(&b'#') => 2,
            b'_' => 1,
            _ => 0,
        };
        if sep > 0 {
            parts.push(&s[start..i]);
            i += sep;
            start = i;
        } else {
            i += 1;
        }
    }
    parts.push(&s[start..]);
    while parts.last() == Some(&"") {
        parts.pop();
    }
    parts
}

/// `toString()` (also its JSON form): `ll`, `ll_RR` or `ll_RR_Ssss`.
impl fmt::Display for Locale {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.language)?;
        if let Some(r) = &self.region {
            write!(f, "_{r}")?;
            if let Some(s) = &self.script {
                write!(f, "_{s}")?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn show(s: &str) -> String {
        Locale::parse(s).map_or_else(|_| "<invalid>".into(), |l| l.to_string())
    }

    /// Measured on Java via `/api/me/settings/keyDbLocale?translate=true&locale=...` (invalid
    /// values show the default, `en`); `""` cannot be sent that way and follows from the code.
    #[test]
    fn parse_like_java() {
        for (input, want) in [
            ("fr", "fr"),
            ("FR", "<invalid>"),
            ("iw", "he"),
            ("in", "id"),
            ("ji", "yi"),
            ("fr_CA", "fr_CA"),
            ("fr-CA", "fr_CA"),
            ("fr-ca", "<invalid>"),
            ("x y", "<invalid>"),
            ("ÉT", "<invalid>"),
            ("en__x", "<invalid>"),
            ("_", "<invalid>"),
            ("zh_Hant_TW", "zh_TW_Hant"),
            ("zh_TW_#Hant", "zh_TW_Hant"),
            ("zh-Hant-TW", "zh_TW_Hant"),
            ("es_419", "es_419"),
            ("en_US_", "en_US"),
            ("en_US__", "en_US"),
            ("en_US_POSIX", "<invalid>"),
            ("haw", "haw"),
            ("", "<invalid>"),
            ("e", "<invalid>"),
            ("fr_CA_Latn_x", "<invalid>"),
        ] {
            assert_eq!(show(input), want, "{input:?}");
        }
    }
}
