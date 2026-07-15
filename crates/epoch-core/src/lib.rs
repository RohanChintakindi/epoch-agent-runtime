//! Core domain types for the Epoch runtime.

mod event;
mod id;
mod state;

pub use event::{Event, EventActor, EventKind, EventStatus, InvalidEventKind};
pub use id::{BranchId, CapabilityId, EffectId, EpochId, EventId, SessionId};
pub use state::{BranchState, SessionState, TransitionError};
