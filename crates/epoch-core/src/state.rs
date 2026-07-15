use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    Created,
    Starting,
    Running,
    Suspended,
    Checkpointing,
    Restoring,
    Completed,
    Failed,
}

impl SessionState {
    #[must_use]
    pub const fn can_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Created, Self::Starting | Self::Failed)
                | (Self::Starting, Self::Running | Self::Failed)
                | (
                    Self::Running,
                    Self::Suspended
                        | Self::Checkpointing
                        | Self::Restoring
                        | Self::Completed
                        | Self::Failed
                )
                | (
                    Self::Suspended,
                    Self::Running | Self::Restoring | Self::Completed | Self::Failed
                )
                | (
                    Self::Checkpointing | Self::Restoring,
                    Self::Running | Self::Suspended | Self::Failed
                )
        )
    }

    /// Moves this session into `next` when the lifecycle permits it.
    ///
    /// # Errors
    ///
    /// Returns [`TransitionError`] when the requested transition is not part of the session
    /// lifecycle.
    pub fn transition_to(&mut self, next: Self) -> Result<(), TransitionError<Self>> {
        if self.can_transition_to(next) {
            *self = next;
            Ok(())
        } else {
            Err(TransitionError {
                current: *self,
                requested: next,
            })
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BranchState {
    Created,
    Running,
    Suspended,
    Completed,
    Promoted,
    Abandoned,
    Failed,
}

impl BranchState {
    #[must_use]
    pub const fn can_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (
                Self::Created,
                Self::Running | Self::Abandoned | Self::Failed
            ) | (
                Self::Running,
                Self::Suspended | Self::Completed | Self::Abandoned | Self::Failed
            ) | (
                Self::Suspended,
                Self::Running | Self::Completed | Self::Abandoned | Self::Failed
            ) | (Self::Completed, Self::Promoted | Self::Abandoned)
        )
    }

    /// Moves this branch into `next` when the lifecycle permits it.
    ///
    /// # Errors
    ///
    /// Returns [`TransitionError`] when the requested transition is not part of the branch
    /// lifecycle.
    pub fn transition_to(&mut self, next: Self) -> Result<(), TransitionError<Self>> {
        if self.can_transition_to(next) {
            *self = next;
            Ok(())
        } else {
            Err(TransitionError {
                current: *self,
                requested: next,
            })
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error("invalid state transition from {current:?} to {requested:?}")]
pub struct TransitionError<State> {
    pub current: State,
    pub requested: State,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_happy_path_is_valid() {
        let mut state = SessionState::Created;
        for next in [
            SessionState::Starting,
            SessionState::Running,
            SessionState::Checkpointing,
            SessionState::Suspended,
            SessionState::Restoring,
            SessionState::Running,
            SessionState::Completed,
        ] {
            state.transition_to(next).expect("valid transition");
        }
        assert_eq!(state, SessionState::Completed);
    }

    #[test]
    fn terminal_session_states_cannot_transition() {
        for terminal in [SessionState::Completed, SessionState::Failed] {
            for next in [
                SessionState::Created,
                SessionState::Starting,
                SessionState::Running,
                SessionState::Suspended,
                SessionState::Checkpointing,
                SessionState::Restoring,
                SessionState::Completed,
                SessionState::Failed,
            ] {
                assert!(!terminal.can_transition_to(next));
            }
        }
    }

    #[test]
    fn invalid_session_transition_preserves_state() {
        let mut state = SessionState::Created;
        let error = state
            .transition_to(SessionState::Completed)
            .expect_err("transition must fail");
        assert_eq!(state, SessionState::Created);
        assert_eq!(error.current, SessionState::Created);
        assert_eq!(error.requested, SessionState::Completed);
    }

    #[test]
    fn branch_can_be_promoted_only_after_completion() {
        let mut branch = BranchState::Created;
        assert!(branch.transition_to(BranchState::Promoted).is_err());
        branch
            .transition_to(BranchState::Running)
            .expect("start branch");
        branch
            .transition_to(BranchState::Completed)
            .expect("complete branch");
        branch
            .transition_to(BranchState::Promoted)
            .expect("promote branch");
        assert_eq!(branch, BranchState::Promoted);
    }

    #[test]
    fn abandoned_branch_is_terminal() {
        let mut branch = BranchState::Running;
        branch
            .transition_to(BranchState::Abandoned)
            .expect("abandon branch");
        assert!(branch.transition_to(BranchState::Running).is_err());
    }
}
