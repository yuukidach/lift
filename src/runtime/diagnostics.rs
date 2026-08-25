use std::collections::VecDeque;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracing::warn;

use crate::common::config::DiagnosticsSettings;
use crate::core::ids::{DisplayId, WindowId, WorkspaceId};
use crate::core::snapshot::CoreSnapshot;
use crate::model::reactor::Command;

const MAX_CAPTURED_WINDOWS: usize = 512;
const MAX_APP_NAME_CHARS: usize = 128;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UserInputTrace {
    pub source: String,
    pub input: String,
    pub command: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
struct DiagnosticState {
    focused_window: Option<WindowId>,
    displays: Vec<DisplayState>,
    workspaces: Vec<WorkspaceState>,
    windows: Vec<WindowState>,
    total_window_count: usize,
    windows_truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
struct DisplayState {
    id: DisplayId,
    space: Option<crate::core::ids::SpaceId>,
    is_active_context: bool,
    active_workspace: Option<WorkspaceId>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
struct WorkspaceState {
    id: WorkspaceId,
    number: Option<u8>,
    name: String,
    display: DisplayId,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
struct WindowState {
    id: WindowId,
    application_name: Option<String>,
    workspace: Option<WorkspaceId>,
    floating: bool,
    minimized: bool,
    fullscreen: bool,
}

impl DiagnosticState {
    fn from_snapshot(snapshot: &CoreSnapshot) -> Self {
        let total_window_count = snapshot.windows.len();
        let windows = snapshot
            .windows
            .iter()
            .take(MAX_CAPTURED_WINDOWS)
            .map(|window| WindowState {
                id: window.id,
                application_name: window.application_name.as_deref().map(truncate_app_name),
                workspace: window.workspace,
                floating: window.floating,
                minimized: window.minimized,
                fullscreen: window.fullscreen,
            })
            .collect();
        Self {
            focused_window: snapshot.focused_window,
            displays: snapshot
                .displays
                .iter()
                .map(|display| DisplayState {
                    id: display.id.clone(),
                    space: display.space,
                    is_active_context: display.is_active_context,
                    active_workspace: display.active_workspace,
                })
                .collect(),
            workspaces: snapshot
                .workspaces
                .iter()
                .map(|workspace| WorkspaceState {
                    id: workspace.id,
                    number: workspace.number.map(crate::core::ids::WorkspaceNumber::get),
                    name: workspace.name.clone(),
                    display: workspace.display.clone(),
                })
                .collect(),
            windows,
            total_window_count,
            windows_truncated: total_window_count > MAX_CAPTURED_WINDOWS,
        }
    }
}

fn truncate_app_name(name: &str) -> String { name.chars().take(MAX_APP_NAME_CHARS).collect() }

struct PendingOperation {
    timestamp_ms: u128,
    input: Option<UserInputTrace>,
    command: Value,
    before: DiagnosticState,
    decisions: Vec<Value>,
}

pub struct DiagnosticLog {
    settings: DiagnosticsSettings,
    path: PathBuf,
    writer: Option<RollingWriter>,
    pending_input: Option<UserInputTrace>,
    pending_operation: Option<PendingOperation>,
    last_observed_state: Option<DiagnosticState>,
    sequence: u64,
}

impl DiagnosticLog {
    pub fn new(settings: DiagnosticsSettings, path: PathBuf) -> Self {
        let writer = open_writer(&settings, &path);
        Self {
            settings,
            path,
            writer,
            pending_input: None,
            pending_operation: None,
            last_observed_state: None,
            sequence: 0,
        }
    }

    pub fn reconfigure(&mut self, settings: DiagnosticsSettings) {
        if self.settings == settings {
            return;
        }
        self.settings = settings;
        self.writer = open_writer(&self.settings, &self.path);
    }

    pub fn note_input(&mut self, input: UserInputTrace) {
        if self.settings.enabled {
            self.pending_input = Some(input);
        }
    }

    pub fn begin_operation(&mut self, command: &Command, before: &CoreSnapshot) {
        if !self.settings.enabled {
            return;
        }
        self.pending_operation = Some(PendingOperation {
            timestamp_ms: timestamp_ms(),
            input: self.pending_input.take(),
            command: serde_json::to_value(command)
                .unwrap_or_else(|error| json!({"serialization_error": error.to_string()})),
            before: DiagnosticState::from_snapshot(before),
            decisions: Vec::new(),
        });
    }

    pub fn record_decision(&mut self, name: &str, details: Value) {
        let Some(operation) = self.pending_operation.as_mut() else {
            return;
        };
        operation.decisions.push(json!({"name": name, "details": details}));
    }

    pub fn finish_operation(&mut self, after: &CoreSnapshot) {
        let Some(operation) = self.pending_operation.take() else {
            return;
        };
        self.write(json!({
            "version": 1,
            "timestamp_ms": operation.timestamp_ms,
            "kind": "operation",
            "input": operation.input,
            "command": operation.command,
            "decisions": operation.decisions,
            "before": operation.before,
            "after": DiagnosticState::from_snapshot(after),
        }));
    }

    pub fn record_snapshot(&mut self, snapshot: &CoreSnapshot) {
        if !self.settings.enabled {
            return;
        }
        let state = DiagnosticState::from_snapshot(snapshot);
        if self.last_observed_state.as_ref() == Some(&state) {
            return;
        }
        self.last_observed_state = Some(state.clone());
        self.write(json!({
            "version": 1,
            "timestamp_ms": timestamp_ms(),
            "kind": "observed_state",
            "snapshot_revision": snapshot.revision,
            "state": state,
        }));
    }

    fn write(&mut self, mut record: Value) {
        self.sequence = self.sequence.saturating_add(1);
        if let Some(object) = record.as_object_mut() {
            object.insert("sequence".into(), Value::from(self.sequence));
        }
        let Some(writer) = self.writer.as_mut() else {
            return;
        };
        if let Err(error) = writer.write_json(&record) {
            warn!(?error, path = ?self.path, "Could not write Lift diagnostic log");
            self.writer = None;
        }
    }
}

fn open_writer(settings: &DiagnosticsSettings, path: &Path) -> Option<RollingWriter> {
    if !settings.enabled {
        return None;
    }
    match RollingWriter::open(
        path.to_path_buf(),
        settings.max_file_size_mb.saturating_mul(1024 * 1024),
        settings.retained_files,
    ) {
        Ok(writer) => Some(writer),
        Err(error) => {
            warn!(?error, ?path, "Could not open Lift diagnostic log");
            None
        }
    }
}

fn timestamp_ms() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis()
}

struct RollingWriter {
    path: PathBuf,
    file: File,
    len: u64,
    max_bytes: u64,
    retained_files: usize,
}

impl RollingWriter {
    fn open(path: PathBuf, max_bytes: u64, retained_files: usize) -> io::Result<Self> {
        let parent = path.parent().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "diagnostic path has no parent")
        })?;
        fs::create_dir_all(parent)?;
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let len = file.metadata()?.len();
        Ok(Self {
            path,
            file,
            len,
            max_bytes: max_bytes.max(1),
            retained_files: retained_files.max(1),
        })
    }

    fn write_json(&mut self, value: &Value) -> io::Result<()> {
        let mut encoded = serde_json::to_vec(value).map_err(io::Error::other)?;
        encoded.push(b'\n');
        if self.len > 0 && self.len.saturating_add(encoded.len() as u64) > self.max_bytes {
            self.rotate()?;
        }
        self.file.write_all(&encoded)?;
        self.file.flush()?;
        self.len = self.len.saturating_add(encoded.len() as u64);
        Ok(())
    }

    fn rotate(&mut self) -> io::Result<()> {
        self.file.flush()?;
        for index in (1..self.retained_files).rev() {
            let source = if index == 1 {
                self.path.clone()
            } else {
                rotated_path(&self.path, index - 1)
            };
            let destination = rotated_path(&self.path, index);
            match fs::rename(&source, &destination) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        if self.retained_files == 1 {
            match fs::remove_file(&self.path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        self.file = OpenOptions::new().create(true).write(true).truncate(true).open(&self.path)?;
        self.len = 0;
        Ok(())
    }
}

fn rotated_path(path: &Path, index: usize) -> PathBuf {
    let mut name: OsString = path.as_os_str().to_owned();
    name.push(format!(".{index}"));
    PathBuf::from(name)
}

pub fn tail(path: &Path, lines: usize) -> io::Result<Vec<String>> {
    if lines == 0 {
        return Ok(Vec::new());
    }
    let mut output = VecDeque::with_capacity(lines);
    let mut backups = Vec::new();
    for index in 1.. {
        let backup = rotated_path(path, index);
        if !backup.exists() {
            break;
        }
        backups.push(backup);
    }
    backups.reverse();
    backups.push(path.to_path_buf());
    for file in backups {
        let reader = BufReader::new(File::open(file)?);
        for line in reader.lines() {
            if output.len() == lines {
                output.pop_front();
            }
            output.push_back(line?);
        }
    }
    Ok(output.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rolling_writer_caps_the_number_and_size_of_files() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("operations.jsonl");
        let mut writer = RollingWriter::open(path.clone(), 80, 3).unwrap();
        for index in 0..20 {
            writer.write_json(&json!({"index": index, "padding": "1234567890"})).unwrap();
        }

        assert!(path.exists());
        assert!(rotated_path(&path, 1).exists());
        assert!(rotated_path(&path, 2).exists());
        assert!(!rotated_path(&path, 3).exists());
        for file in [&path, &rotated_path(&path, 1), &rotated_path(&path, 2)] {
            assert!(fs::metadata(file).unwrap().len() <= 80);
        }
    }

    #[test]
    fn tail_returns_only_the_most_recent_lines() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("operations.jsonl");
        fs::write(&path, "one\ntwo\nthree\n").unwrap();
        assert_eq!(tail(&path, 2).unwrap(), vec!["two", "three"]);
    }

    #[test]
    fn tail_reads_across_rotated_files_in_chronological_order() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("operations.jsonl");
        fs::write(rotated_path(&path, 2), "one\n").unwrap();
        fs::write(rotated_path(&path, 1), "two\n").unwrap();
        fs::write(&path, "three\n").unwrap();
        assert_eq!(tail(&path, 3).unwrap(), vec!["one", "two", "three"]);
    }

    #[test]
    fn operation_records_input_decisions_and_omits_window_titles() {
        use std::num::NonZeroU32;

        use crate::core::geometry::Rect;
        use crate::core::ids::ApplicationId;
        use crate::core::snapshot::WindowSnapshot;
        use crate::model::layout::LayoutCommand;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("operations.jsonl");
        let mut log = DiagnosticLog::new(
            DiagnosticsSettings {
                enabled: true,
                max_file_size_mb: 1,
                retained_files: 2,
            },
            path.clone(),
        );
        let window = WindowId::new(ApplicationId(7), NonZeroU32::new(9).unwrap());
        let mut snapshot = CoreSnapshot::default();
        snapshot.focused_window = Some(window);
        snapshot.windows.push(WindowSnapshot {
            id: window,
            workspace: None,
            frame: Rect::new(0.0, 0.0, 100.0, 100.0).unwrap(),
            title: "private document title".into(),
            application_name: Some("Editor".into()),
            platform_id: Some(42),
            floating: false,
            minimized: false,
            fullscreen: false,
        });
        log.note_input(UserInputTrace {
            source: "hotkey".into(),
            input: "Shift + Meta + 3".into(),
            command: json!({"move_window_to_workspace": 2}),
        });
        log.begin_operation(
            &Command::Layout(LayoutCommand::MoveWindowToWorkspace {
                workspace: 2,
                window_id: None,
            }),
            &snapshot,
        );
        log.record_decision("move_window_resolution", json!({"resolved_window": window}));
        log.finish_operation(&snapshot);

        let encoded = fs::read_to_string(path).unwrap();
        let record: Value = serde_json::from_str(encoded.trim()).unwrap();
        assert_eq!(record["kind"], "operation");
        assert_eq!(record["input"]["source"], "hotkey");
        assert_eq!(record["decisions"][0]["name"], "move_window_resolution");
        assert!(encoded.contains("Editor"));
        assert!(!encoded.contains("private document title"));
    }
}
