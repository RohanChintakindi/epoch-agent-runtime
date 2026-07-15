//! Integrity-checked, content-addressed blob storage for Epoch artifacts.

use std::{
    fmt,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    str::FromStr,
};

use serde::{Deserialize, Deserializer, Serialize, de};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

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
    /// Returns an error when the store directories cannot be created.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, BlobError> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(root.join("sha256/.tmp"))?;
        Ok(Self { root })
    }

    /// Returns the canonical final path for a blob hash.
    #[must_use]
    pub fn blob_path(&self, hash: &BlobHash) -> PathBuf {
        self.root
            .join("sha256")
            .join(&hash.as_str()[..2])
            .join(hash.as_str())
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

        if final_path.try_exists()? {
            self.read(&hash)?;
            return Ok(metadata);
        }

        let shard_directory = self.root.join("sha256").join(&hash.as_str()[..2]);
        fs::create_dir_all(&shard_directory)?;

        let temp_directory = self.root.join("sha256/.tmp");
        let temp_path = temp_directory.join(format!("{hash}.{}.tmp", Uuid::new_v4()));
        let temp_blob = TemporaryBlob::create(temp_path)?;
        temp_blob.write_and_sync(bytes)?;

        // A concurrent writer may have published the same address while this file was written.
        // Verify that winner instead of replacing a potentially corrupted trusted file.
        if final_path.try_exists()? {
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
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(BlobError::NotFound(hash.clone()));
            }
            Err(error) => return Err(error.into()),
        };
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
}

impl TemporaryBlob {
    fn create(path: PathBuf) -> Result<Self, BlobError> {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        Ok(Self { path })
    }

    fn write_and_sync(&self, bytes: &[u8]) -> Result<(), BlobError> {
        let mut file = OpenOptions::new().write(true).open(&self.path)?;
        file.write_all(bytes)?;
        file.sync_all()?;
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

fn sync_directory(path: &Path) -> Result<(), BlobError> {
    File::open(path)?.sync_all()?;
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
}
