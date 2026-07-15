//! Direct-process supervision for observable Epoch workloads.

mod manifest;
mod stream;
mod supervisor;

pub use manifest::{
    MAX_ARGUMENT_BYTES, MAX_ARGUMENTS, MAX_MANIFEST_BYTES, ManifestError, WorkloadManifest,
};
pub use supervisor::{
    AgentTermination, BranchStatus, DirectSupervisor, EventPageReport, EventPageRequest,
    ExecutionError, InspectionError, MAX_STDERR_BYTES, MAX_STDOUT_BYTES, ObservedEvent, RunOutcome,
    SessionStatusReport, SupervisorError,
};
