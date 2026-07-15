//! Integrity-checked, content-addressed blob storage for Epoch artifacts.

use std::{
    fmt,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    str::FromStr,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};

use serde::{Deserialize, Deserializer, Serialize, de};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

#[cfg(unix)]
const PRIVATE_DIRECTORY_MODE: u32 = 0o700;
#[cfg(unix)]
const PRIVATE_FILE_MODE: u32 = 0o600;
const DEFAULT_STALE_TEMPORARY_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// A canonical lowercase SHA-256 digest.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct BlobHash(String);

impl BlobHash {
    /// Computes the address for `bytes`.
    #[must_use]
    pub fn digest(bytes: &[u8]) -> Self {
        const HEX: &[u8; 16] = b"0123456789abcdef";

        let digest = Sha256::digest(bytes);
        let mut encoded = String::with_capacity(digest.len() * 2);
        for byte in digest {
            encoded.push(char::from(HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
        Self(encoded)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for BlobHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl FromStr for BlobHash {
    type Err = InvalidBlobHash;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            Ok(Self(value.to_owned()))
        } else {
            Err(InvalidBlobHash(value.to_owned()))
        }
    }
}

impl<'de> Deserialize<'de> for BlobHash {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(de::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("invalid canonical SHA-256 blob hash: {0:?}")]
pub struct InvalidBlobHash(String);

/// Metadata stored by the trusted database alongside a filesystem blob.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BlobMetadata {
    pub hash: BlobHash,
    pub length: u64,
    pub media_type: String,
}

/// Filesystem-backed content-addressed storage.
#[derive(Debug)]
pub struct BlobStore {
    root: PathBuf,
}

impl BlobStore {
    /// Creates or opens a blob store rooted at `root`.
    ///
    /// # Errors
    ///
    /// Returns an error when the store directories cannot be created or when an existing managed
    /// path is a symlink, has the wrong type, or is accessible to other Unix users. The caller is
    /// responsible for choosing a trusted parent directory; Epoch never follows symlinks at the
    /// blob root or below it.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, BlobError> {
        let root = root.as_ref().to_path_buf();
        let root_created = ensure_secure_directory(&root)?;
        if root_created && let Some(parent) = root.parent() {
            // The caller explicitly supplies and trusts the parent. It may itself be a platform
            // alias such as macOS `/tmp`, so the managed-tree no-follow policy begins at `root`.
            sync_trusted_directory(parent)?;
        }

        let sha256 = root.join("sha256");
        if ensure_secure_directory(&sha256)? {
            sync_directory(&root)?;
        }
        if ensure_secure_directory(&sha256.join(".tmp"))? {
            sync_directory(&sha256)?;
        }
        let store = Self { root };
        store.cleanup_stale_temporary_files(DEFAULT_STALE_TEMPORARY_AGE)?;
        Ok(store)
    }

    /// Returns the canonical final path for a blob hash.
    #[must_use]
    pub fn blob_path(&self, hash: &BlobHash) -> PathBuf {
        self.root
            .join("sha256")
            .join(&hash.as_str()[..2])
            .join(hash.as_str())
    }

    /// Removes temporary files created by interrupted Epoch writes once they reach `minimum_age`.
    ///
    /// Only files with Epoch's hash-and-UUID temporary naming format are eligible. Unknown files
    /// are retained, and any symlink in the managed temporary directory is rejected rather than
    /// followed or silently removed.
    ///
    /// # Errors
    ///
    /// Returns an error when the temporary directory cannot be inspected or safely synchronized.
    pub fn cleanup_stale_temporary_files(&self, minimum_age: Duration) -> Result<usize, BlobError> {
        let temp_directory = self.root.join("sha256/.tmp");
        let metadata = fs::symlink_metadata(&temp_directory)?;
        validate_directory(&temp_directory, &metadata)?;

        let cutoff = SystemTime::now()
            .checked_sub(minimum_age)
            .unwrap_or(UNIX_EPOCH);
        let mut removed = 0;
        for entry in fs::read_dir(&temp_directory)? {
            let entry = entry?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                return Err(BlobError::UnsafeSymlink { path });
            }
            if !is_managed_temporary_name(&entry.file_name()) {
                continue;
            }
            if !metadata.is_file() {
                return Err(BlobError::UnexpectedFileType {
                    path,
                    expected: "regular file",
                });
            }
            if metadata.modified()? <= cutoff {
                fs::remove_file(path)?;
                removed += 1;
            }
        }
        if removed != 0 {
            sync_directory(&temp_directory)?;
        }
        Ok(removed)
    }

    /// Persists bytes and returns metadata suitable for the trusted database.
    ///
    /// # Errors
    ///
    /// Returns an error on filesystem or integrity failures.
    pub fn put(
        &self,
        bytes: &[u8],
        media_type: impl Into<String>,
    ) -> Result<BlobMetadata, BlobError> {
        let hash = BlobHash::digest(bytes);
        let length = u64::try_from(bytes.len()).map_err(|_| BlobError::BlobTooLarge {
            length: bytes.len(),
        })?;
        let metadata = BlobMetadata {
            hash: hash.clone(),
            length,
            media_type: media_type.into(),
        };
        let final_path = self.blob_path(&hash);

        if secure_regular_file_exists(&final_path)? {
            self.read(&hash)?;
            return Ok(metadata);
        }

        let shard_directory = self.root.join("sha256").join(&hash.as_str()[..2]);
        if ensure_secure_directory(&shard_directory)? {
            // Persist the new shard name in its parent before publishing a blob into it.
            sync_directory(&self.root.join("sha256"))?;
        }

        let temp_directory = self.root.join("sha256/.tmp");
        let temp_path = temp_directory.join(format!("{hash}.{}.tmp", Uuid::new_v4()));
        let mut temp_blob = TemporaryBlob::create(temp_path)?;
        temp_blob.write_and_sync(bytes)?;

        // A concurrent writer may have published the same address while this file was written.
        // Verify that winner instead of replacing a potentially corrupted trusted file.
        if secure_regular_file_exists(&final_path)? {
            self.read(&hash)?;
            return Ok(metadata);
        }

        fs::rename(temp_blob.path(), &final_path)?;
        sync_directory(&shard_directory)?;
        sync_directory(&temp_directory)?;
        Ok(metadata)
    }

    /// Reads and verifies a blob before returning any bytes.
    ///
    /// # Errors
    ///
    /// Returns an error when the blob is missing, unreadable, or does not match its address.
    pub fn read(&self, hash: &BlobHash) -> Result<Vec<u8>, BlobError> {
        let path = self.blob_path(hash);
        let mut file = match open_secure_regular_file(&path) {
            Ok(file) => file,
            Err(BlobError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(BlobError::NotFound(hash.clone()));
            }
            Err(error) => return Err(error),
        };
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        let actual = BlobHash::digest(&bytes);
        if actual == *hash {
            Ok(bytes)
        } else {
            Err(BlobError::HashMismatch {
                expected: hash.clone(),
                actual,
            })
        }
    }
}

#[derive(Debug)]
struct TemporaryBlob {
    path: PathBuf,
    file: File,
}

impl TemporaryBlob {
    fn create(path: PathBuf) -> Result<Self, BlobError> {
        let file = create_secure_file(&path)?;
        Ok(Self { path, file })
    }

    fn write_and_sync(&mut self, bytes: &[u8]) -> Result<(), BlobError> {
        self.file.write_all(bytes)?;
        self.file.sync_all()?;
        Ok(())
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TemporaryBlob {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn ensure_secure_directory(path: &Path) -> Result<bool, BlobError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            validate_directory(path, &metadata)?;
            Ok(false)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match create_secure_directory(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
            let metadata = fs::symlink_metadata(path)?;
            validate_directory(path, &metadata)?;
            Ok(true)
        }
        Err(error) => Err(error.into()),
    }
}

fn is_managed_temporary_name(name: &std::ffi::OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    let Some(stem) = name.strip_suffix(".tmp") else {
        return false;
    };
    let Some((hash, identifier)) = stem.split_once('.') else {
        return false;
    };
    BlobHash::from_str(hash).is_ok() && Uuid::parse_str(identifier).is_ok()
}

fn validate_directory(path: &Path, metadata: &fs::Metadata) -> Result<(), BlobError> {
    if metadata.file_type().is_symlink() {
        return Err(BlobError::UnsafeSymlink {
            path: path.to_path_buf(),
        });
    }
    if !metadata.is_dir() {
        return Err(BlobError::UnexpectedFileType {
            path: path.to_path_buf(),
            expected: "directory",
        });
    }
    validate_directory_permissions(path, metadata)
}

fn validate_regular_file(path: &Path, metadata: &fs::Metadata) -> Result<(), BlobError> {
    if metadata.file_type().is_symlink() {
        return Err(BlobError::UnsafeSymlink {
            path: path.to_path_buf(),
        });
    }
    if !metadata.is_file() {
        return Err(BlobError::UnexpectedFileType {
            path: path.to_path_buf(),
            expected: "regular file",
        });
    }
    validate_file_permissions(path, metadata)
}

#[cfg(unix)]
fn validate_directory_permissions(path: &Path, metadata: &fs::Metadata) -> Result<(), BlobError> {
    validate_unix_permissions(path, metadata, PRIVATE_DIRECTORY_MODE)
}

#[cfg(not(unix))]
fn validate_directory_permissions(_path: &Path, _metadata: &fs::Metadata) -> Result<(), BlobError> {
    Ok(())
}

#[cfg(unix)]
fn validate_file_permissions(path: &Path, metadata: &fs::Metadata) -> Result<(), BlobError> {
    validate_unix_permissions(path, metadata, PRIVATE_FILE_MODE)
}

#[cfg(not(unix))]
fn validate_file_permissions(_path: &Path, _metadata: &fs::Metadata) -> Result<(), BlobError> {
    Ok(())
}

#[cfg(unix)]
fn validate_unix_permissions(
    path: &Path,
    metadata: &fs::Metadata,
    expected: u32,
) -> Result<(), BlobError> {
    let actual = metadata.mode() & 0o7777;
    if actual == expected {
        Ok(())
    } else {
        Err(BlobError::InsecurePermissions {
            path: path.to_path_buf(),
            expected,
            actual,
        })
    }
}

#[cfg(unix)]
fn create_secure_directory(path: &Path) -> std::io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.mode(PRIVATE_DIRECTORY_MODE).create(path)?;
    let directory = open_directory_no_follow(path)?;
    directory.set_permissions(fs::Permissions::from_mode(PRIVATE_DIRECTORY_MODE))
}

#[cfg(not(unix))]
fn create_secure_directory(path: &Path) -> std::io::Result<()> {
    fs::create_dir(path)
}

#[cfg(unix)]
fn create_secure_file(path: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(PRIVATE_FILE_MODE)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(not(unix))]
fn create_secure_file(path: &Path) -> std::io::Result<File> {
    OpenOptions::new().write(true).create_new(true).open(path)
}

fn secure_regular_file_exists(path: &Path) -> Result<bool, BlobError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            validate_regular_file(path, &metadata)?;
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn open_secure_regular_file(path: &Path) -> Result<File, BlobError> {
    let metadata = fs::symlink_metadata(path)?;
    validate_regular_file(path, &metadata)?;
    let file = open_regular_file_no_follow(path)?;
    validate_regular_file(path, &file.metadata()?)?;
    Ok(file)
}

#[cfg(unix)]
fn open_regular_file_no_follow(path: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(not(unix))]
fn open_regular_file_no_follow(path: &Path) -> std::io::Result<File> {
    File::open(path)
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), BlobError> {
    open_directory_no_follow(path)?.sync_all()?;
    Ok(())
}

#[cfg(unix)]
fn open_directory_no_follow(path: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<(), BlobError> {
    Ok(())
}

#[cfg(unix)]
fn sync_trusted_directory(path: &Path) -> Result<(), BlobError> {
    File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn sync_trusted_directory(_path: &Path) -> Result<(), BlobError> {
    Ok(())
}

#[derive(Debug, Error)]
pub enum BlobError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("blob length {length} cannot be represented as u64")]
    BlobTooLarge { length: usize },
    #[error("blob {0} was not found")]
    NotFound(BlobHash),
    #[error("blob integrity mismatch: expected {expected}, computed {actual}")]
    HashMismatch {
        expected: BlobHash,
        actual: BlobHash,
    },
    #[error("unsafe symlink in blob store at {path:?}")]
    UnsafeSymlink { path: PathBuf },
    #[error("unexpected object at {path:?}; expected a {expected}")]
    UnexpectedFileType {
        path: PathBuf,
        expected: &'static str,
    },
    #[error("insecure Unix permissions at {path:?}: expected {expected:#o}, found {actual:#o}")]
    InsecurePermissions {
        path: PathBuf,
        expected: u32,
        actual: u32,
    },
}
