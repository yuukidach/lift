use std::num::NonZeroU32;

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const WORKSPACE_SLOTS: usize = WorkspaceNumber::MAX as usize;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct ApplicationId(pub i32);

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct DisplayId(pub String);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct WorkspaceId(pub u64);

impl std::fmt::Display for WorkspaceId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{:08}", self.0)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct GroupId(pub u64);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct SpaceId(pub u64);

#[derive(
    Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize,
)]
pub struct Generation(pub u64);

#[derive(
    Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize,
)]
pub struct TransactionId(pub u64);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct EffectId(pub u64);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct WindowId {
    pub application: ApplicationId,
    pub index: NonZeroU32,
}

impl WindowId {
    pub const fn new(application: ApplicationId, index: NonZeroU32) -> Self {
        Self { application, index }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct WorkspaceNumber(u8);

impl WorkspaceNumber {
    pub const MIN: u8 = 1;
    pub const MAX: u8 = 10;

    pub const fn get(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for WorkspaceNumber {
    type Error = WorkspaceNumberError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        if (Self::MIN..=Self::MAX).contains(&value) {
            Ok(Self(value))
        } else {
            Err(WorkspaceNumberError(value))
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error("workspace number must be in 1..=10, got {0}")]
pub struct WorkspaceNumberError(pub u8);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_number_accepts_only_global_slots() {
        assert_eq!(WorkspaceNumber::try_from(1).unwrap().get(), 1);
        assert_eq!(WorkspaceNumber::try_from(10).unwrap().get(), 10);
        assert_eq!(WorkspaceNumber::try_from(0), Err(WorkspaceNumberError(0)));
        assert_eq!(WorkspaceNumber::try_from(11), Err(WorkspaceNumberError(11)));
    }

    #[test]
    fn window_identity_includes_application_and_index() {
        let index = NonZeroU32::new(7).unwrap();
        let first = WindowId::new(ApplicationId(41), index);
        let second = WindowId::new(ApplicationId(42), index);
        assert_ne!(first, second);
    }
}
