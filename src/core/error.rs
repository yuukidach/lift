use thiserror::Error;

use super::ids::{EffectId, Generation, WindowId, WorkspaceId};

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum CoreError {
    #[error("invalid command: {0}")]
    InvalidCommand(String),
    #[error("window is not known: {0:?}")]
    MissingWindow(WindowId),
    #[error("workspace state conflicts with {0:?}")]
    WorkspaceConflict(WorkspaceId),
    #[error("stale generation: expected {expected:?}, received {received:?}")]
    StaleGeneration {
        expected: Generation,
        received: Generation,
    },
    #[error("unsupported command: {0}")]
    UnsupportedCommand(String),
    #[error("incomplete observation: {0}")]
    IncompleteObservation(String),
    #[error("platform effect {effect:?} failed: {message}")]
    PlatformEffectFailed { effect: EffectId, message: String },
    #[error("core invariant violated: {0}")]
    InvariantViolation(String),
}
