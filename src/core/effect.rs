use serde::{Deserialize, Serialize};

use super::geometry::{Point, Rect};
use super::ids::{
    ApplicationId, DisplayId, EffectId, Generation, SpaceId, TransactionId, WindowId, WorkspaceId,
};
use super::snapshot::{PersistedState, UiSnapshot};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    ApplyLayout(LayoutRequest),
    FocusWindow(WindowId),
    RaiseWindow(WindowId),
    RefreshWindows(RefreshRequest),
    CloseWindow(WindowId),
    SwitchNativeSpace(NativeSpaceRequest),
    WarpPointer(Point),
    SetPointerHidden(bool),
    Haptic(HapticKind),
    UpdateUi(UiSnapshot),
    Publish(DomainEvent),
    RunCommand(ExternalCommand),
    Save(PersistedState),
    Shutdown(ShutdownReason),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LayoutRequest {
    pub workspace: WorkspaceId,
    pub frames: Vec<WindowFrame>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowFrame {
    pub window: WindowId,
    pub frame: Rect,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RefreshRequest {
    pub application: Option<ApplicationId>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NativeSpaceRequest {
    pub display: DisplayId,
    pub space: SpaceId,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExternalCommand {
    pub program: String,
    pub arguments: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HapticKind {
    Alignment,
    LevelChange,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShutdownReason {
    Requested,
    InvariantViolation,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectOutcome {
    Succeeded,
    Failed { code: String, message: String },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EffectCompletion {
    pub effect_id: EffectId,
    pub transaction: TransactionId,
    pub generation: Generation,
    pub outcome: EffectOutcome,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DomainEvent {
    SnapshotPublished {
        revision: u64,
    },
    WorkspaceChanged {
        display: DisplayId,
        workspace: WorkspaceId,
    },
    FocusChanged {
        window: Option<WindowId>,
    },
}
