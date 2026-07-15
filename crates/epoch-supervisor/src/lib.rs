//! Direct-process supervision for observable Epoch workloads.

mod manifest;

pub use manifest::{
    MAX_ARGUMENT_BYTES, MAX_ARGUMENTS, MAX_MANIFEST_BYTES, ManifestError, WorkloadManifest,
};
