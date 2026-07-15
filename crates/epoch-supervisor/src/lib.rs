//! Direct-process supervision for observable Epoch workloads.

mod manifest;
mod stream;
mod supervisor;

pub use manifest::{
    MAX_ARGUMENT_BYTES, MAX_ARGUMENTS, MAX_MANIFEST_BYTES, ManifestError, WorkloadManifest,
};
pub use supervisor::{
    AgentTermination, DirectSupervisor, ExecutionError, MAX_STDERR_BYTES, MAX_STDOUT_BYTES,
    RunOutcome, SupervisorError,
};
