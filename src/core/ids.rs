use std::num::NonZeroU32;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

pub const WORKSPACE_SLOTS: usize = WorkspaceNumber::COUNT;

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

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct WorkspaceNumber(u8);

impl WorkspaceNumber {
    pub const MIN: u8 = 0;
    pub const MAX: u8 = 9;
    pub const COUNT: usize = 10;
    pub const ORDERED: [Self; Self::COUNT] = [
        Self(1),
        Self(2),
        Self(3),
        Self(4),
        Self(5),
        Self(6),
        Self(7),
        Self(8),
        Self(9),
        Self(0),
    ];

    pub const fn get(self) -> u8 {
        self.0
    }

    pub const fn from_global_slot(slot: usize) -> Option<Self> {
        if slot < 9 {
            Some(Self(slot as u8 + 1))
        } else if slot == 9 {
            Some(Self(0))
        } else {
            None
        }
    }

    pub const fn global_slot(self) -> usize {
        if self.0 == 0 { 9 } else { self.0 as usize - 1 }
    }
}

impl Ord for WorkspaceNumber {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.global_slot().cmp(&other.global_slot())
    }
}

impl PartialOrd for WorkspaceNumber {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Serialize for WorkspaceNumber {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u8(self.0)
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

impl<'de> Deserialize<'de> for WorkspaceNumber {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = u8::deserialize(deserializer)?;
        Self::try_from(value).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error("workspace number must be in 0..=9, got {0}")]
pub struct WorkspaceNumberError(pub u8);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_number_accepts_only_global_slots() {
        assert_eq!(WorkspaceNumber::try_from(0).unwrap().get(), 0);
        assert_eq!(WorkspaceNumber::try_from(1).unwrap().get(), 1);
        assert_eq!(WorkspaceNumber::try_from(9).unwrap().get(), 9);
        assert_eq!(WorkspaceNumber::try_from(10), Err(WorkspaceNumberError(10)));
        assert_eq!(WorkspaceNumber::try_from(11), Err(WorkspaceNumberError(11)));
    }

    #[test]
    fn workspace_number_order_matches_the_digit_row() {
        let mut numbers = (0..=9)
            .map(|number| WorkspaceNumber::try_from(number).unwrap())
            .collect::<Vec<_>>();
        numbers.sort();
        assert_eq!(numbers, WorkspaceNumber::ORDERED);
        assert_eq!(WorkspaceNumber::from_global_slot(0).unwrap().get(), 1);
        assert_eq!(WorkspaceNumber::from_global_slot(9).unwrap().get(), 0);
        assert_eq!(WorkspaceNumber::try_from(0).unwrap().global_slot(), 9);
    }

    #[test]
    fn window_identity_includes_application_and_index() {
        let index = NonZeroU32::new(7).unwrap();
        let first = WindowId::new(ApplicationId(41), index);
        let second = WindowId::new(ApplicationId(42), index);
        assert_ne!(first, second);
    }
}
