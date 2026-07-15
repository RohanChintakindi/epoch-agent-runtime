use std::{
    fs::{self, File},
    io::Read as _,
    path::{Path, PathBuf},
};

use serde::Deserialize;
use thiserror::Error;

pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;
pub const MAX_ARGUMENTS: usize = 128;
pub const MAX_ARGUMENT_BYTES: usize = 4096;
const MAX_MANIFEST_READ_BYTES: u64 = 64 * 1024 + 1;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkloadManifest {
    pub name: String,
    pub executable: PathBuf,
    pub arguments: Vec<String>,
    pub working_directory: PathBuf,
}

impl WorkloadManifest {
    /// Loads and validates a bounded version 1 workload manifest.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the manifest cannot be read or violates the direct backend's
    /// schema and path constraints.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ManifestError> {
        let requested_path = path.as_ref();
        let manifest_path =
            fs::canonicalize(requested_path).map_err(|source| ManifestError::Io {
                path: requested_path.to_path_buf(),
                source,
            })?;
        let mut file = File::open(&manifest_path).map_err(|source| ManifestError::Io {
            path: manifest_path.clone(),
            source,
        })?;
        let mut encoded = Vec::with_capacity(MAX_MANIFEST_BYTES.min(8192));
        file.by_ref()
            .take(MAX_MANIFEST_READ_BYTES)
            .read_to_end(&mut encoded)
            .map_err(|source| ManifestError::Io {
                path: manifest_path.clone(),
                source,
            })?;
        if encoded.len() > MAX_MANIFEST_BYTES {
            return Err(ManifestError::TooLarge {
                maximum: MAX_MANIFEST_BYTES,
            });
        }
        let text = std::str::from_utf8(&encoded).map_err(|error| ManifestError::Parse {
            message: error.to_string(),
        })?;
        let raw: RawManifest = toml::from_str(text).map_err(|error| ManifestError::Parse {
            message: error.to_string(),
        })?;
        raw.validate(&manifest_path)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawManifest {
    schema_version: u16,
    name: String,
    executable: String,
    #[serde(default)]
    arguments: Vec<String>,
    working_directory: Option<String>,
}

impl RawManifest {
    fn validate(self, manifest_path: &Path) -> Result<WorkloadManifest, ManifestError> {
        if self.schema_version != 1 {
            return Err(ManifestError::UnsupportedVersion {
                received: self.schema_version,
            });
        }
        if self.name.is_empty()
            || self.name.len() > 128
            || !self
                .name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        {
            return Err(ManifestError::InvalidName);
        }
        if self.arguments.len() > MAX_ARGUMENTS {
            return Err(ManifestError::TooManyArguments {
                actual: self.arguments.len(),
                maximum: MAX_ARGUMENTS,
            });
        }
        for (index, argument) in self.arguments.iter().enumerate() {
            if argument.len() > MAX_ARGUMENT_BYTES || argument.contains('\0') {
                return Err(ManifestError::InvalidArgument {
                    index,
                    maximum: MAX_ARGUMENT_BYTES,
                });
            }
        }

        let base = manifest_path
            .parent()
            .ok_or_else(|| ManifestError::InvalidPath {
                value: manifest_path.display().to_string(),
            })?;
        let executable = canonical_relative(base, &self.executable)?;
        validate_executable(&executable)?;
        let working_directory = self.working_directory.map_or_else(
            || Ok(base.to_path_buf()),
            |path| canonical_relative(base, &path),
        )?;
        if !working_directory.is_dir() {
            return Err(ManifestError::InvalidWorkingDirectory {
                path: working_directory,
            });
        }

        Ok(WorkloadManifest {
            name: self.name,
            executable,
            arguments: self.arguments,
            working_directory,
        })
    }
}

fn canonical_relative(base: &Path, value: &str) -> Result<PathBuf, ManifestError> {
    if value.is_empty() || value.contains('\0') {
        return Err(ManifestError::InvalidPath {
            value: value.to_owned(),
        });
    }
    let path = Path::new(value);
    let candidate = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    };
    fs::canonicalize(&candidate).map_err(|source| ManifestError::Io {
        path: candidate,
        source,
    })
}

#[cfg(unix)]
fn validate_executable(path: &Path) -> Result<(), ManifestError> {
    use std::os::unix::fs::PermissionsExt as _;

    let metadata = fs::metadata(path).map_err(|source| ManifestError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.is_file() && metadata.permissions().mode() & 0o111 != 0 {
        Ok(())
    } else {
        Err(ManifestError::InvalidExecutable {
            path: path.to_path_buf(),
        })
    }
}

#[cfg(not(unix))]
fn validate_executable(path: &Path) -> Result<(), ManifestError> {
    if path.is_file() {
        Ok(())
    } else {
        Err(ManifestError::InvalidExecutable {
            path: path.to_path_buf(),
        })
    }
}

#[derive(Debug, Error)]
pub enum ManifestError {
    #[error("workload manifest is larger than {maximum} bytes")]
    TooLarge { maximum: usize },
    #[error("workload manifest is invalid: {message}")]
    Parse { message: String },
    #[error("unsupported workload manifest version {received}; expected 1")]
    UnsupportedVersion { received: u16 },
    #[error("workload name is invalid")]
    InvalidName,
    #[error("workload has {actual} arguments; maximum is {maximum}")]
    TooManyArguments { actual: usize, maximum: usize },
    #[error("workload argument {index} is invalid or larger than {maximum} bytes")]
    InvalidArgument { index: usize, maximum: usize },
    #[error("workload executable is not a regular executable file: {path}")]
    InvalidExecutable { path: PathBuf },
    #[error("workload working directory is not a directory: {path}")]
    InvalidWorkingDirectory { path: PathBuf },
    #[error("workload path is invalid: {value:?}")]
    InvalidPath { value: String },
    #[error("workload manifest I/O failed for {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}
