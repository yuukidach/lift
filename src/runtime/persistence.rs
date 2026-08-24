use std::fs;
use std::io;
use std::path::Path;

use crate::core::snapshot::PersistedState;

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
    ron::de::from_str(&encoded).map_err(io::Error::other)
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
            schema_version: 1,
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
}
