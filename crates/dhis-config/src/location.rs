//! Port of `org.hisp.dhis.external.location.DefaultLocationManager`.
//!
//! Java resolves DHIS2_HOME from, in order: the JNDI context variable `dhis2-home`, the system
//! property `dhis2.home`, the env var `DHIS2_HOME`, then `/opt/dhis2`. A candidate is only used if
//! it is a writable directory or can be created (the check creates it as a side effect, as in
//! Java). There is no JNDI or JVM system property in Rust: `--dhis2-home` stands in for
//! `-Ddhis2.home` (see docs/deviations.md).

use std::path::{Component, Path, PathBuf};

use rustix::fs::{Access, access};
use thiserror::Error;
use tracing::{error, info, warn};

pub const DEFAULT_DHIS2_HOME: &str = "/opt/dhis2";
pub const DEFAULT_ENV_VAR: &str = "DHIS2_HOME";
pub const DEFAULT_SYS_PROP: &str = "dhis2.home";

/// `LocationManagerException`.
#[derive(Debug, Error, PartialEq, Eq)]
#[error("{0}")]
pub struct LocationManagerException(pub String);

#[derive(Debug, Clone)]
pub struct LocationManager {
    external_dir: Option<PathBuf>,
}

impl LocationManager {
    /// `DefaultLocationManager#init`.
    ///
    /// `system_property` is the Rust stand-in for `-Ddhis2.home`; `env_var` is the value of
    /// `DHIS2_HOME`, passed in so tests do not depend on the process environment.
    pub fn init(system_property: Option<&str>, env_var: Option<&str>) -> Self {
        let external_dir = Self::from_sys_property(system_property)
            .or_else(|| Self::from_env(env_var))
            .or_else(Self::from_default);
        Self { external_dir }
    }

    /// A manager pinned to a directory, like `setExternalDir` in Java tests.
    pub fn with_external_dir(dir: impl Into<PathBuf>) -> Self {
        Self {
            external_dir: Some(dir.into()),
        }
    }

    fn from_sys_property(path: Option<&str>) -> Option<PathBuf> {
        match path {
            Some(p) => {
                info!("System property {DEFAULT_SYS_PROP} points to {p}");
                directory_is_valid(Path::new(p)).then(|| PathBuf::from(p))
            }
            None => {
                info!("System property {DEFAULT_SYS_PROP} not set");
                None
            }
        }
    }

    fn from_env(path: Option<&str>) -> Option<PathBuf> {
        match path {
            Some(p) => {
                info!("Environment variable {DEFAULT_ENV_VAR} points to {p}");
                directory_is_valid(Path::new(p)).then(|| PathBuf::from(p))
            }
            None => {
                info!("Environment variable {DEFAULT_ENV_VAR} not set");
                None
            }
        }
    }

    fn from_default() -> Option<PathBuf> {
        if directory_is_valid(Path::new(DEFAULT_DHIS2_HOME)) {
            info!("Home directory set to {DEFAULT_DHIS2_HOME}");
            Some(PathBuf::from(DEFAULT_DHIS2_HOME))
        } else {
            error!("No Home directory set, and {DEFAULT_DHIS2_HOME} is not a directory");
            None
        }
    }

    pub fn external_directory(&self) -> Result<&Path, LocationManagerException> {
        self.external_dir
            .as_deref()
            .ok_or_else(|| LocationManagerException("External directory not set".into()))
    }

    pub fn external_directory_set(&self) -> bool {
        self.external_dir.is_some()
    }

    /// `getFileForReading(fileName, directories...)`.
    pub fn file_for_reading(
        &self,
        file_name: &str,
        directories: &[&str],
    ) -> Result<PathBuf, LocationManagerException> {
        let directory = self.build_directory(directories)?;
        let file = directory.join(file_name);
        validate_path_within_bounds(&file, &directory)?;
        if !can_read_file(&file) {
            return Err(LocationManagerException(format!(
                "File {} cannot be read",
                absolute(&file).display()
            )));
        }
        Ok(file)
    }

    /// `buildDirectory(directories...)`.
    pub fn build_directory(
        &self,
        directories: &[&str],
    ) -> Result<PathBuf, LocationManagerException> {
        let mut dir = self.external_directory()?.to_path_buf();
        for d in directories {
            dir.push(d);
        }
        Ok(dir)
    }
}

/// `directoryIsValid`: writable if it exists, otherwise try `mkdirs`.
fn directory_is_valid(dir: &Path) -> bool {
    if dir.exists() {
        if access(dir, Access::WRITE_OK).is_err() {
            info!("Directory {} is not writeable", absolute(dir).display());
            return false;
        }
        true
    } else if std::fs::create_dir_all(dir).is_err() {
        info!("Directory {} cannot be created", absolute(dir).display());
        false
    } else {
        true
    }
}

fn can_read_file(file: &Path) -> bool {
    if !file.exists() {
        info!("File {} does not exist", absolute(file).display());
        return false;
    }
    if access(file, Access::READ_OK).is_err() {
        info!("File {} cannot be read", absolute(file).display());
        return false;
    }
    true
}

fn validate_path_within_bounds(file: &Path, base: &Path) -> Result<(), LocationManagerException> {
    let canonical_file = canonical_path(file);
    let canonical_base = canonical_path(base);
    if canonical_file != canonical_base && !canonical_file.starts_with(&canonical_base) {
        warn!(
            "Path traversal attempt detected: {} is outside of base directory {}",
            canonical_file.display(),
            canonical_base.display()
        );
        return Err(LocationManagerException(
            "Invalid file path: path traversal attempt detected".into(),
        ));
    }
    Ok(())
}

/// `File#getCanonicalPath`: resolves symlinks for the existing prefix and normalises the rest
/// lexically (Java does not require the file to exist).
fn canonical_path(path: &Path) -> PathBuf {
    let abs = absolute(path);
    let mut existing = abs.clone();
    let mut rest: Vec<std::ffi::OsString> = Vec::new();
    loop {
        if let Ok(c) = existing.canonicalize() {
            let mut out = c;
            for part in rest.iter().rev() {
                out.push(part);
            }
            return normalize(&out);
        }
        match (existing.file_name(), existing.parent()) {
            (Some(name), Some(parent)) => {
                rest.push(name.to_os_string());
                existing = parent.to_path_buf();
            }
            _ => return normalize(&abs),
        }
    }
}

fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sys_property_beats_env() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let lm = LocationManager::init(a.path().to_str(), b.path().to_str());
        assert_eq!(lm.external_directory().unwrap(), a.path());
        let lm = LocationManager::init(None, b.path().to_str());
        assert_eq!(lm.external_directory().unwrap(), b.path());
    }

    #[test]
    fn missing_home_is_created_like_java_mkdirs() {
        let a = tempfile::tempdir().unwrap();
        let home = a.path().join("nested/home");
        let lm = LocationManager::init(None, home.to_str());
        assert_eq!(lm.external_directory().unwrap(), home);
        assert!(home.is_dir());
    }

    #[test]
    fn reading_rejects_traversal_and_missing_files() {
        let a = tempfile::tempdir().unwrap();
        std::fs::write(a.path().join("dhis.conf"), "x=1").unwrap();
        let lm = LocationManager::with_external_dir(a.path());
        assert!(lm.file_for_reading("dhis.conf", &[]).is_ok());
        assert_eq!(
            lm.file_for_reading("../etc/passwd", &[]).unwrap_err().0,
            "Invalid file path: path traversal attempt detected"
        );
        assert!(
            lm.file_for_reading("nope.conf", &[])
                .unwrap_err()
                .0
                .ends_with("nope.conf cannot be read")
        );
    }
}
