//! `java.util.Date` handling as DHIS2 renders it.
//!
//! Dates are epoch milliseconds; Java renders them in the JVM default time zone. That zone is
//! resolved the way the JDK does on Linux/macOS (`TimeZone_md.c`), which differs from other
//! libraries: `TZ`, then `/etc/timezone`, then the `/etc/localtime` symlink target (or an
//! identical file under the zoneinfo directory), and **`GMT`** when nothing is found. The DHIS2
//! server image has neither `TZ` nor `/etc/localtime`, so Java reports `GMT` there (measured).
//! The tz database is bundled, as the JVM carries its own.

use std::path::Path;
use std::sync::LazyLock;

use jiff::tz::TimeZone;
use jiff::{Timestamp, Zoned};

/// `TimeZone.getDefault().getID()` as the JVM would determine it.
pub static SYSTEM_TZ_ID: LazyLock<String> = LazyLock::new(|| {
    jvm_default_tz_id(
        std::env::var("TZ").ok().as_deref(),
        Path::new("/etc/timezone"),
        Path::new("/etc/localtime"),
        Path::new("/usr/share/zoneinfo"),
    )
});

/// The server time zone, as the JVM's `TimeZone.getDefault()` would pick it.
pub static SYSTEM_TZ: LazyLock<TimeZone> =
    LazyLock::new(|| TimeZone::get(&SYSTEM_TZ_ID).unwrap_or(TimeZone::UTC));

/// The JDK's default zone lookup (Unix). Inputs are injectable for tests.
pub fn jvm_default_tz_id(
    tz_env: Option<&str>,
    etc_timezone: &Path,
    localtime: &Path,
    zoneinfo: &Path,
) -> String {
    if let Some(tz) = tz_env
        .map(|t| t.trim_start_matches(':'))
        .filter(|t| !t.is_empty())
    {
        return tz.to_string();
    }
    if let Ok(content) = std::fs::read_to_string(etc_timezone)
        && let Some(line) = content
            .lines()
            .next()
            .map(str::trim)
            .filter(|l| !l.is_empty())
    {
        return line.to_string();
    }
    if let Ok(target) = std::fs::read_link(localtime) {
        let t = target.to_string_lossy();
        if let Some(i) = t.find("zoneinfo/") {
            return t[i + "zoneinfo/".len()..].to_string();
        }
    }
    if let Ok(bytes) = std::fs::read(localtime)
        && let Some(id) = find_identical(zoneinfo, zoneinfo, &bytes)
    {
        return id;
    }
    "GMT".to_string()
}

fn find_identical(root: &Path, dir: &Path, wanted: &[u8]) -> Option<String> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .collect();
    entries.sort();
    for p in entries {
        let name = p.file_name()?.to_string_lossy().to_string();
        // The JDK skips these helper files and links.
        if name.starts_with('.')
            || ["posixrules", "localtime", "posix", "right"].contains(&name.as_str())
        {
            continue;
        }
        if p.is_dir() {
            if let Some(found) = find_identical(root, &p, wanted) {
                return Some(found);
            }
        } else if std::fs::read(&p).ok().as_deref() == Some(wanted) {
            return p
                .strip_prefix(root)
                .ok()
                .map(|r| r.to_string_lossy().to_string());
        }
    }
    None
}

static DISPLAY_NAMES: LazyLock<std::collections::BTreeMap<String, String>> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../../data/tz-display-names.json"))
        .expect("generated tz-display-names.json")
});

/// `TimeZone#getID()` of the server zone.
pub fn system_tz_id() -> String {
    SYSTEM_TZ_ID.clone()
}

/// `TimeZone#getDisplayName()` (standard time, long, English), from the JDK of the server image.
pub fn tz_display_name(id: &str) -> Option<&'static str> {
    DISPLAY_NAMES.get(id).map(String::as_str)
}

/// `System.currentTimeMillis()`.
pub fn now_millis() -> i64 {
    Timestamp::now().as_millisecond()
}

/// DHIS2's JSON date format: `yyyy-MM-dd'T'HH:mm:ss.SSS` in the server time zone.
pub fn format_iso_millis(epoch_millis: i64) -> String {
    format_iso_millis_in(epoch_millis, &SYSTEM_TZ)
}

pub fn format_iso_millis_in(epoch_millis: i64, tz: &TimeZone) -> String {
    match Timestamp::from_millisecond(epoch_millis) {
        Ok(ts) => {
            let z: Zoned = ts.to_zoned(tz.clone());
            z.strftime("%Y-%m-%dT%H:%M:%S.%3f").to_string()
        }
        Err(_) => String::new(),
    }
}

/// `LocalDateTime.parse(raw)` interpreted in `tz` (`ISO_LOCAL_DATE_TIME`: seconds and fraction
/// optional).
pub fn parse_local_date_time(raw: &str, tz: &TimeZone) -> Option<i64> {
    let dt: jiff::civil::DateTime = raw.parse().ok()?;
    if !raw.contains('T') {
        return None;
    }
    Some(dt.to_zoned(tz.clone()).ok()?.timestamp().as_millisecond())
}

/// `DateUtils.getPrettyInterval(start, end)`: Joda `Period(end - start)` printed with
/// `"%d h, %d m, %d s"` fields. A period from a duration only has precise fields (hours and
/// below); zero fields are skipped, an all-zero period prints `0 s`, and one that only has
/// milliseconds prints an empty string (verified against the JDK).
pub fn pretty_interval(start_millis: i64, end_millis: i64) -> String {
    let diff = end_millis - start_millis;
    let hours = diff / 3_600_000;
    let minutes = (diff % 3_600_000) / 60_000;
    let seconds = (diff % 60_000) / 1000;
    let millis = diff % 1000;
    let mut parts = Vec::new();
    if hours != 0 {
        parts.push(format!("{hours} h"));
    }
    if minutes != 0 {
        parts.push(format!("{minutes} m"));
    }
    if seconds != 0 {
        parts.push(format!("{seconds} s"));
    }
    if parts.is_empty() && millis == 0 {
        return "0 s".into();
    }
    parts.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `DateUtils.getPrettyInterval` outputs from the JDK (jshell).
    #[test]
    fn pretty_interval_matches_jdk() {
        let base = 1_000_000_000_000;
        for (diff, want) in [
            (0, "0 s"),
            (999, ""),
            (1000, "1 s"),
            (59_999, "59 s"),
            (60_000, "1 m"),
            (61_000, "1 m, 1 s"),
            (3_600_000, "1 h"),
            (3_661_001, "1 h, 1 m, 1 s"),
            (86_400_000, "24 h"),
            (24_754_961_000, "6876 h, 22 m, 41 s"),
            (1_792_065_000_000, "497795 h, 50 m"),
            (-1, ""),
            (-1000, "-1 s"),
            (-3_661_000, "-1 h, -1 m, -1 s"),
            (7_200_000, "2 h"),
            (7_200_500, "2 h"),
            (120_000, "2 m"),
        ] {
            assert_eq!(pretty_interval(base, base + diff), want, "{diff}");
        }
    }

    #[test]
    fn jvm_zone_resolution() {
        let none = Path::new("/nonexistent/x");
        assert_eq!(
            jvm_default_tz_id(Some("Europe/Oslo"), none, none, none),
            "Europe/Oslo"
        );
        assert_eq!(jvm_default_tz_id(Some(":UTC"), none, none, none), "UTC");
        // Nothing configured: the JDK falls back to GMT (measured in the server image).
        assert_eq!(jvm_default_tz_id(None, none, none, none), "GMT");
        assert_eq!(tz_display_name("GMT"), Some("Greenwich Mean Time"));
        let dir = std::env::temp_dir().join(format!("tzt{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("timezone"), "Africa/Lagos\n").unwrap();
        assert_eq!(
            jvm_default_tz_id(None, &dir.join("timezone"), none, none),
            "Africa/Lagos"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn formatting_and_parsing() {
        let tz = TimeZone::get("Africa/Maseru").unwrap();
        assert_eq!(format_iso_millis_in(0, &tz), "1970-01-01T02:00:00.000");
        let ms = parse_local_date_time("2026-01-02T03:04:05.006", &tz).unwrap();
        assert_eq!(format_iso_millis_in(ms, &tz), "2026-01-02T03:04:05.006");
        assert!(parse_local_date_time("2026-01-02", &tz).is_none());
        assert_eq!(
            tz_display_name("Africa/Maseru"),
            Some("South Africa Standard Time")
        );
    }
}
