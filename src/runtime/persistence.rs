use std::fs;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::core::ids::{DisplayId, WorkspaceId, WorkspaceNumber};
use crate::core::snapshot::{PersistedState, PersistedWorkspace};

const CURRENT_SCHEMA_VERSION: u16 = 2;

#[derive(Deserialize, Serialize)]
struct PersistedStateWire {
    schema_version: u16,
    workspaces: Vec<PersistedWorkspaceWire>,
}

#[derive(Deserialize, Serialize)]
struct PersistedWorkspaceWire {
    id: WorkspaceId,
    number: PersistedNumberWire,
    display: DisplayId,
}

#[derive(Deserialize, Serialize)]
#[serde(untagged)]
enum PersistedNumberWire {
    Scalar(u8),
    LegacyNewtype((u8,)),
}

impl PersistedNumberWire {
    fn get(self) -> u8 {
        match self {
            Self::Scalar(number) | Self::LegacyNewtype((number,)) => number,
        }
    }
}

pub fn save(path: &Path, state: &PersistedState) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "persistence path has no parent")
    })?;
    fs::create_dir_all(parent)?;
    let temporary = path.with_extension("ron.tmp");
    let encoded = ron::ser::to_string_pretty(state, ron::ser::PrettyConfig::default())
        .map_err(io::Error::other)?;
    fs::write(&temporary, encoded)?;
    fs::rename(temporary, path)
}

pub fn load(path: &Path) -> io::Result<PersistedState> {
    let encoded = fs::read_to_string(path)?;
    let wire: PersistedStateWire = ron::de::from_str(&encoded).map_err(io::Error::other)?;
    if !matches!(wire.schema_version, 1 | CURRENT_SCHEMA_VERSION) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported persisted state schema {}", wire.schema_version),
        ));
    }
    let workspaces = wire
        .workspaces
        .into_iter()
        .map(|workspace| {
            let number = workspace.number.get();
            let number = if wire.schema_version == 1 && number == 10 {
                0
            } else {
                number
            };
            Ok(PersistedWorkspace {
                id: workspace.id,
                number: WorkspaceNumber::try_from(number).map_err(io::Error::other)?,
                display: workspace.display,
            })
        })
        .collect::<io::Result<Vec<_>>>()?;
    Ok(PersistedState { schema_version: CURRENT_SCHEMA_VERSION, workspaces })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ids::{DisplayId, WorkspaceId, WorkspaceNumber};

    #[test]
    fn persisted_state_is_written_atomically_and_round_trips() {
        let directory = std::env::temp_dir().join(format!(
            "lift-persistence-test-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("thread")
        ));
        let path = directory.join("layout.ron");
        let state = PersistedState {
            schema_version: CURRENT_SCHEMA_VERSION,
            workspaces: vec![crate::core::snapshot::PersistedWorkspace {
                id: WorkspaceId(7),
                number: WorkspaceNumber::try_from(3).unwrap(),
                display: DisplayId("main".into()),
            }],
        };

        save(&path, &state).unwrap();
        assert_eq!(load(&path).unwrap(), state);

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn schema_one_workspace_ten_migrates_to_zero() {
        let directory = std::env::temp_dir().join(format!(
            "lift-persistence-migration-test-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("thread")
        ));
        let path = directory.join("layout.ron");
        std::fs::create_dir_all(&directory).unwrap();
        let legacy = PersistedStateWire {
            schema_version: 1,
            workspaces: vec![PersistedWorkspaceWire {
                id: WorkspaceId(7),
                number: PersistedNumberWire::LegacyNewtype((10,)),
                display: DisplayId("main".into()),
            }],
        };
        std::fs::write(
            &path,
            ron::ser::to_string_pretty(&legacy, ron::ser::PrettyConfig::default()).unwrap(),
        )
        .unwrap();

        let migrated = load(&path).unwrap();

        assert_eq!(migrated.schema_version, CURRENT_SCHEMA_VERSION);
        assert_eq!(migrated.workspaces[0].number.get(), 0);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
