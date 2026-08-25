use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

mod reducer;

use super::config::CoreConfig;
use super::effect::{DomainEvent, Effect, EffectCompletion};
use super::error::CoreError;
use super::ids::{DisplayId, Generation, TransactionId, WindowId, WorkspaceId};
use super::input::{DisplayObservation, Input, WindowObservation};
use super::interaction::{DragSwapState, MissionControlPhase};
use super::rules::RuleSet;
use super::snapshot::{
    CoreSnapshot, DisplaySnapshot, PersistedState, PersistedWorkspace, WindowSnapshot,
};
use super::workspace::WorkspaceCatalog;

#[derive(Clone)]
pub struct CoreState {
    platform: PlatformState,
    workspaces: WorkspaceCatalog,
    focus: FocusState,
    interactions: InteractionState,
    config: CoreConfig,
    rules: RuleSet,
    revision: u64,
}

#[derive(Clone, Default)]
struct PlatformState {
    generation: Generation,
    displays: BTreeMap<DisplayId, DisplayObservation>,
    display_order: Vec<DisplayId>,
    active_display: Option<DisplayId>,
    windows: BTreeMap<WindowId, WindowObservation>,
    managed: BTreeSet<WindowId>,
}

#[derive(Clone, Default)]
struct FocusState {
    focused_window: Option<WindowId>,
    last_tiled_by_workspace: BTreeMap<WorkspaceId, WindowId>,
    last_floating_by_workspace: BTreeMap<WorkspaceId, WindowId>,
}

#[derive(Clone, Default)]
struct InteractionState {
    current_transaction: TransactionId,
    drag: DragSwapState,
    mission_control: MissionControlPhase,
}

impl CoreState {
    pub fn new(config: CoreConfig) -> Self {
        let rules = RuleSet::compile(config.window_rules.clone())
            .expect("CoreConfig passed to CoreState must already be validated");
        Self {
            platform: PlatformState::default(),
            workspaces: WorkspaceCatalog::default(),
            focus: FocusState::default(),
            interactions: InteractionState::default(),
            config,
            rules,
            revision: 0,
        }
    }

    pub fn from_persisted(
        config: CoreConfig,
        persisted: &PersistedState,
    ) -> Result<Self, CoreError> {
        if persisted.schema_version != 2 {
            return Err(CoreError::InvalidCommand(format!(
                "unsupported persisted state schema {}",
                persisted.schema_version
            )));
        }
        let rules = RuleSet::compile(config.window_rules.clone())?;
        Ok(Self {
            platform: PlatformState::default(),
            workspaces: WorkspaceCatalog::from_persisted(&persisted.workspaces)?,
            focus: FocusState::default(),
            interactions: InteractionState::default(),
            config,
            rules,
            revision: 0,
        })
    }

    pub fn config(&self) -> &CoreConfig { &self.config }

    pub fn snapshot(&self) -> Arc<CoreSnapshot> {
        let displays = self
            .platform
            .display_order
            .iter()
            .filter_map(|id| self.platform.displays.get(id))
            .map(|display| DisplaySnapshot {
                id: display.id.clone(),
                frame: display.frame,
                space: display.space,
                is_active_context: self.platform.active_display.as_ref() == Some(&display.id),
                active_workspace: self.workspaces.active_workspace(&display.id),
                last_workspace: self.workspaces.last_workspace(&display.id),
            })
            .collect();
        let display_frames = self
            .platform
            .displays
            .values()
            .map(|display| (display.id.clone(), display.frame))
            .collect();
        let window_constraints = self
            .platform
            .windows
            .values()
            .map(|window| (window.id, window.constraints))
            .collect();
        let mut workspaces = self
            .workspaces
            .snapshots_with_layout(&display_frames, &self.config.layout, &window_constraints)
            .expect("committed workspace state must satisfy BSP invariants");
        for workspace in &mut workspaces {
            workspace.last_tiled_window =
                self.focus.last_tiled_by_workspace.get(&workspace.id).copied();
            workspace.last_floating_window =
                self.focus.last_floating_by_workspace.get(&workspace.id).copied();
        }
        let windows = self
            .platform
            .windows
            .values()
            .filter(|window| self.platform.managed.contains(&window.id))
            .map(|window| {
                let workspace = self.workspaces.workspace_for_window(window.id);
                let floating = self.workspaces.is_floating(window.id);
                WindowSnapshot {
                    id: window.id,
                    workspace,
                    frame: window.frame,
                    title: window.title.clone(),
                    application_name: window.app_name.clone(),
                    platform_id: window.platform_id,
                    floating,
                    minimized: window.minimized,
                    fullscreen: window.fullscreen,
                }
            })
            .collect();

        Arc::new(CoreSnapshot {
            revision: self.revision,
            platform_generation: self.platform.generation,
            displays,
            workspaces,
            windows,
            applications: Vec::new(),
            focused_window: self.focus.focused_window,
            drag: self.interactions.drag.snapshot(),
            mission_control: self.interactions.mission_control,
        })
    }

    pub fn completion_is_current(&self, completion: &EffectCompletion) -> bool {
        completion.generation == self.platform.generation
            && completion.transaction == self.interactions.current_transaction
    }

    fn persisted_state(&self) -> PersistedState {
        PersistedState {
            schema_version: 2,
            workspaces: self
                .snapshot()
                .workspaces
                .iter()
                .map(|workspace| PersistedWorkspace {
                    id: workspace.id,
                    number: workspace.number,
                    display: workspace.display.clone(),
                })
                .collect(),
        }
    }
}

pub fn transition(state: &mut CoreState, input: Input) -> Result<Transition, CoreError> {
    state.transition(input)
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ChangeSet {
    pub displays: BTreeSet<DisplayId>,
    pub workspaces: BTreeSet<WorkspaceId>,
    pub windows: BTreeSet<WindowId>,
    pub focus_changed: bool,
    pub config_changed: bool,
    pub ui_changed: bool,
}

#[derive(Clone, Debug)]
pub struct Transition {
    pub transaction: TransactionId,
    pub changes: ChangeSet,
    pub effects: Vec<Effect>,
    pub events: Vec<DomainEvent>,
    pub snapshot: Arc<CoreSnapshot>,
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use super::*;
    use crate::core::command::{
        Command, DisplayCommand, MissionControlCommand, WindowCommand, WorkspaceCommand,
    };
    use crate::core::constraints::WindowConstraints;
    use crate::core::effect::{Effect, EffectCompletion, EffectOutcome};
    use crate::core::geometry::{Rect, Size};
    use crate::core::ids::{
        ApplicationId, EffectId, Generation, SpaceId, TransactionId, WorkspaceNumber,
    };
    use crate::core::input::{
        DisplayObservation, DisplayTopologyObservation, Observation, PlatformSnapshotObservation,
        WindowObservation,
    };
    use crate::core::interaction::{DragCandidate, DragObservation};
    use crate::core::rules::{WindowRule, WorkspaceTarget};

    fn number(value: u8) -> WorkspaceNumber { WorkspaceNumber::try_from(value).unwrap() }

    fn window(index: u32) -> WindowId {
        WindowId::new(ApplicationId(42), NonZeroU32::new(index).unwrap())
    }

    fn display(id: &str, space: u64) -> DisplayObservation {
        DisplayObservation {
            id: DisplayId(id.into()),
            frame: Rect::new(0.0, 0.0, 1200.0, 800.0).unwrap(),
            space: Some(SpaceId(space)),
        }
    }

    fn observed_window(id: WindowId, display: &str) -> WindowObservation {
        WindowObservation {
            id,
            frame: Rect::new(10.0, 20.0, 500.0, 400.0).unwrap(),
            display: Some(DisplayId(display.into())),
            platform_id: Some(id.index.get()),
            app_id: Some("com.example.Terminal".into()),
            app_name: Some("Terminal".into()),
            title: "Shell".into(),
            ax_role: Some("AXWindow".into()),
            ax_subrole: Some("AXStandardWindow".into()),
            minimized: false,
            fullscreen: false,
            constraints: WindowConstraints {
                resizable: true,
                preferred_size: Size { width: 500.0, height: 400.0 },
                min_size: None,
                max_size: None,
            },
        }
    }

    fn observe(
        state: &mut CoreState,
        generation: u64,
        displays: Vec<DisplayObservation>,
        windows: Vec<WindowObservation>,
    ) -> Result<Transition, CoreError> {
        let active_display = displays.first().map(|display| display.id.clone());
        state.transition(Input::Observation(Observation::PlatformSnapshot(
            PlatformSnapshotObservation {
                generation: Generation(generation),
                displays,
                active_display,
                windows,
                focused_window: None,
            },
        )))
    }

    #[test]
    fn new_state_publishes_an_empty_revision_zero_snapshot() {
        let state = CoreState::new(CoreConfig::default());
        let snapshot = state.snapshot();
        assert_eq!(snapshot.revision, 0);
        assert_eq!(snapshot.platform_generation, Generation(0));
        assert!(snapshot.displays.is_empty());
        assert!(snapshot.workspaces.is_empty());
        assert!(snapshot.windows.is_empty());
        assert_eq!(snapshot.focused_window, None);
    }

    #[test]
    fn persisted_state_restores_stable_workspace_identity_before_platform_observation() {
        let persisted = PersistedState {
            schema_version: 2,
            workspaces: vec![
                PersistedWorkspace {
                    id: WorkspaceId(7),
                    number: number(3),
                    display: DisplayId("external".into()),
                },
                PersistedWorkspace {
                    id: WorkspaceId(4),
                    number: number(1),
                    display: DisplayId("main".into()),
                },
            ],
        };
        let mut state = CoreState::from_persisted(CoreConfig::default(), &persisted).unwrap();

        let restored = state.persisted_state();
        assert_eq!(restored.schema_version, 2);
        assert_eq!(restored.workspaces.len(), 2);
        assert!(restored.workspaces.contains(&persisted.workspaces[0]));
        assert!(restored.workspaces.contains(&persisted.workspaces[1]));

        let observed = observe(
            &mut state,
            1,
            vec![display("main", 9), display("external", 10)],
            Vec::new(),
        )
        .unwrap();
        assert!(observed.snapshot.workspaces.iter().any(|workspace| {
            workspace.id == WorkspaceId(7)
                && workspace.number == number(3)
                && workspace.display == DisplayId("external".into())
        }));
    }

    #[test]
    fn persisted_state_rejects_unknown_schema() {
        let error = CoreState::from_persisted(CoreConfig::default(), &PersistedState {
            schema_version: 3,
            workspaces: Vec::new(),
        })
        .err()
        .unwrap();
        assert!(matches!(error, CoreError::InvalidCommand(_)));
    }

    #[test]
    fn serialized_input_sequence_replays_deterministically() {
        let first = window(1);
        let second = window(2);
        let inputs = vec![
            Input::Observation(Observation::PlatformSnapshot(PlatformSnapshotObservation {
                generation: Generation(1),
                displays: vec![display("main", 9)],
                active_display: Some(DisplayId("main".into())),
                windows: vec![
                    observed_window(first, "main"),
                    observed_window(second, "main"),
                ],
                focused_window: Some(first),
            })),
            Input::Command(Command::Window(WindowCommand::ToggleFloating {
                window: Some(first),
            })),
            Input::Command(Command::Workspace(WorkspaceCommand::ActivateOrCreate {
                workspace: number(2),
                display: DisplayId("main".into()),
            })),
            Input::Command(Command::Workspace(WorkspaceCommand::MoveWindow {
                workspace: number(2),
                window: Some(second),
            })),
        ];
        let encoded = ron::ser::to_string(&inputs).unwrap();
        let replayed: Vec<Input> = ron::de::from_str(&encoded).unwrap();

        let run = |inputs: Vec<Input>| {
            let mut state = CoreState::new(CoreConfig::default());
            let transitions = inputs
                .into_iter()
                .map(|input| {
                    let transition = state.transition(input).unwrap();
                    (transition.changes, transition.effects, transition.events)
                })
                .collect::<Vec<_>>();
            (state.snapshot(), transitions)
        };

        let (first_snapshot, first_transitions) = run(inputs);
        let (second_snapshot, second_transitions) = run(replayed);
        assert_eq!(first_snapshot, second_snapshot);
        assert_eq!(first_transitions, second_transitions);
    }

    #[test]
    fn stale_effect_completion_is_not_current() {
        let state = CoreState::new(CoreConfig::default());
        let stale = EffectCompletion {
            effect_id: EffectId(8),
            transaction: TransactionId(3),
            generation: Generation(1),
            outcome: EffectOutcome::Succeeded,
        };
        assert!(!state.completion_is_current(&stale));
    }

    #[test]
    fn platform_snapshot_applies_rule_assignment_and_window_removal_atomically() {
        let mut config = CoreConfig::default();
        config.window_rules.push(WindowRule {
            app_id: Some("com.example.Terminal".into()),
            app_name: None,
            title_regex: None,
            title_substring: None,
            ax_role: None,
            ax_subrole: None,
            workspace: WorkspaceTarget::Number(number(2)),
            floating: true,
            manage: true,
        });
        let mut state = CoreState::new(config);
        let first = observe(&mut state, 1, vec![display("main", 9)], vec![observed_window(
            window(1),
            "main",
        )])
        .unwrap();

        let managed = &first.snapshot.windows[0];
        let workspace = first
            .snapshot
            .workspaces
            .iter()
            .find(|workspace| workspace.number == number(2))
            .unwrap();
        assert_eq!(managed.workspace, Some(workspace.id));
        assert!(managed.floating);
        assert_eq!(workspace.floating_windows, vec![window(1)]);

        let removed = observe(&mut state, 2, vec![display("main", 9)], Vec::new()).unwrap();
        assert!(removed.snapshot.windows.is_empty());
        assert!(
            removed
                .snapshot
                .workspaces
                .iter()
                .all(|workspace| workspace.groups.is_empty()
                    && workspace.floating_windows.is_empty())
        );
    }

    #[test]
    fn current_workspace_rule_does_not_follow_later_workspace_switches() {
        let mut config = CoreConfig::default();
        config.window_rules.push(WindowRule {
            app_id: Some("com.example.Terminal".into()),
            app_name: None,
            title_regex: None,
            title_substring: None,
            ax_role: None,
            ax_subrole: None,
            workspace: WorkspaceTarget::Current,
            floating: false,
            manage: true,
        });
        let mut state = CoreState::new(config);
        let tracked = window(1);
        let initial = observe(&mut state, 1, vec![display("main", 9)], vec![observed_window(
            tracked, "main",
        )])
        .unwrap();
        let original_workspace = initial.snapshot.windows[0].workspace.unwrap();

        state
            .transition(Input::Command(Command::Workspace(
                WorkspaceCommand::ActivateOrCreate {
                    workspace: number(2),
                    display: DisplayId("main".into()),
                },
            )))
            .unwrap();
        let observed_after_switch =
            observe(&mut state, 2, vec![display("main", 9)], vec![observed_window(
                tracked, "main",
            )])
            .unwrap();

        assert_eq!(
            observed_after_switch.snapshot.windows[0].workspace,
            Some(original_workspace)
        );
        assert_eq!(
            observed_after_switch.snapshot.displays[0]
                .active_workspace
                .and_then(|workspace| observed_after_switch
                    .snapshot
                    .workspaces
                    .iter()
                    .find(|candidate| candidate.id == workspace)
                    .map(|workspace| workspace.number)),
            Some(number(2))
        );
    }

    #[test]
    fn stale_display_observation_does_not_undo_cross_display_workspace_move() {
        let mut state = CoreState::new(CoreConfig::default());
        let tracked = window(1);
        let initial = observe(
            &mut state,
            1,
            vec![display("left", 9), display("right", 10)],
            vec![observed_window(tracked, "left")],
        )
        .unwrap();
        let right_workspace = initial
            .snapshot
            .workspaces
            .iter()
            .find(|workspace| workspace.display == DisplayId("right".into()))
            .unwrap();
        let right_number = right_workspace.number;
        let right_id = right_workspace.id;

        state
            .transition(Input::Command(Command::Workspace(
                WorkspaceCommand::MoveWindow {
                    workspace: right_number,
                    window: Some(tracked),
                },
            )))
            .unwrap();

        // AX/window-server geometry may still identify the source display
        // until the asynchronous frame write completes.
        let stale_observation = observe(
            &mut state,
            2,
            vec![display("left", 9), display("right", 10)],
            vec![observed_window(tracked, "left")],
        )
        .unwrap();

        assert_eq!(stale_observation.snapshot.windows[0].workspace, Some(right_id));
    }

    #[test]
    fn repeated_identical_observation_has_an_empty_change_set() {
        let mut state = CoreState::new(CoreConfig::default());
        let initial = observe(&mut state, 1, vec![display("main", 9)], vec![observed_window(
            window(1),
            "main",
        )])
        .unwrap();
        assert_eq!(
            initial.snapshot.workspaces[0].layout_frames[&window(1)],
            Rect::new(0.0, 0.0, 1200.0, 800.0).unwrap()
        );
        assert_eq!(initial.effects, vec![Effect::ApplyLayout(
            crate::core::effect::LayoutRequest {
                workspace: initial.snapshot.workspaces[0].id,
                frames: vec![crate::core::effect::WindowFrame {
                    window: window(1),
                    frame: Rect::new(0.0, 0.0, 1200.0, 800.0).unwrap(),
                }],
            }
        )]);

        let repeated = observe(&mut state, 1, vec![display("main", 9)], vec![observed_window(
            window(1),
            "main",
        )])
        .unwrap();

        assert!(repeated.changes.displays.is_empty());
        assert!(repeated.changes.workspaces.is_empty());
        assert!(repeated.changes.windows.is_empty());
        assert!(!repeated.changes.focus_changed);
        assert!(!repeated.changes.config_changed);
        assert!(!repeated.changes.ui_changed);
    }

    #[test]
    fn unmanaged_rule_keeps_window_out_of_domain_state() {
        let mut config = CoreConfig::default();
        config.window_rules.push(WindowRule {
            app_id: Some("com.example.Terminal".into()),
            app_name: None,
            title_regex: None,
            title_substring: None,
            ax_role: None,
            ax_subrole: None,
            workspace: WorkspaceTarget::Current,
            floating: false,
            manage: false,
        });
        let mut state = CoreState::new(config);
        let transition = observe(&mut state, 1, vec![display("main", 9)], vec![observed_window(
            window(1),
            "main",
        )])
        .unwrap();

        assert!(transition.snapshot.windows.is_empty());
        assert!(transition.snapshot.workspaces[0].groups.is_empty());
    }

    #[test]
    fn incomplete_topology_does_not_partially_commit() {
        let mut state = CoreState::new(CoreConfig::default());
        observe(&mut state, 1, vec![display("main", 9)], Vec::new()).unwrap();
        let before = state.snapshot();

        let error = observe(
            &mut state,
            2,
            vec![display("left", 10), display("right", 10)],
            Vec::new(),
        )
        .unwrap_err();

        assert!(matches!(error, CoreError::IncompleteObservation(_)));
        assert_eq!(state.snapshot().as_ref(), before.as_ref());
    }

    #[test]
    fn removed_display_workspaces_keep_identity_and_follow_migration_priority() {
        let mut config = CoreConfig::default();
        config.display_migration_priority = vec![DisplayId("preferred".into())];
        let mut state = CoreState::new(config);
        let initial = observe(
            &mut state,
            1,
            vec![
                display("departing", 8),
                display("fallback", 9),
                display("preferred", 10),
            ],
            Vec::new(),
        )
        .unwrap();
        let migrating = initial
            .snapshot
            .workspaces
            .iter()
            .find(|workspace| workspace.display.0 == "departing")
            .unwrap()
            .id;

        let committed = state
            .transition(Input::Observation(Observation::DisplayTopology(
                DisplayTopologyObservation {
                    generation: Generation(2),
                    displays: vec![display("fallback", 8), display("preferred", 10)],
                    active_display: Some(DisplayId("fallback".into())),
                },
            )))
            .unwrap();

        assert_eq!(
            committed
                .snapshot
                .workspaces
                .iter()
                .find(|workspace| workspace.id == migrating)
                .unwrap()
                .display,
            DisplayId("preferred".into())
        );
    }

    #[test]
    fn focus_is_committed_with_the_platform_snapshot_or_rejected_atomically() {
        let mut state = CoreState::new(CoreConfig::default());
        let focused = window(1);
        let accepted = state
            .transition(Input::Observation(Observation::PlatformSnapshot(
                PlatformSnapshotObservation {
                    generation: Generation(1),
                    displays: vec![display("main", 9)],
                    active_display: Some(DisplayId("main".into())),
                    windows: vec![observed_window(focused, "main")],
                    focused_window: Some(focused),
                },
            )))
            .unwrap();
        assert_eq!(accepted.snapshot.focused_window, Some(focused));
        assert!(accepted.changes.focus_changed);
        let before = state.snapshot();

        let error = state
            .transition(Input::Observation(Observation::PlatformSnapshot(
                PlatformSnapshotObservation {
                    generation: Generation(2),
                    displays: vec![display("main", 9)],
                    active_display: Some(DisplayId("main".into())),
                    windows: vec![observed_window(focused, "main")],
                    focused_window: Some(window(2)),
                },
            )))
            .unwrap_err();
        assert!(matches!(error, CoreError::IncompleteObservation(_)));
        assert_eq!(state.snapshot().as_ref(), before.as_ref());
    }

    #[test]
    fn focus_observation_updates_workspace_history_without_platform_effects() {
        let mut state = CoreState::new(CoreConfig::default());
        let first = window(1);
        let second = window(2);
        state
            .transition(Input::Observation(Observation::PlatformSnapshot(
                PlatformSnapshotObservation {
                    generation: Generation(1),
                    displays: vec![display("main", 9)],
                    active_display: Some(DisplayId("main".into())),
                    windows: vec![
                        observed_window(first, "main"),
                        observed_window(second, "main"),
                    ],
                    focused_window: Some(first),
                },
            )))
            .unwrap();

        let focused = state
            .transition(Input::Observation(Observation::FocusChanged {
                window: Some(second),
            }))
            .unwrap();

        assert_eq!(focused.snapshot.focused_window, Some(second));
        assert!(
            focused
                .effects
                .iter()
                .all(|effect| !matches!(effect, Effect::FocusWindow(_) | Effect::RaiseWindow(_)))
        );
        assert_eq!(focused.snapshot.workspaces[0].last_tiled_window, Some(second));
    }

    #[test]
    fn active_display_is_published_for_command_adapters() {
        let mut state = CoreState::new(CoreConfig::default());
        let focused = window(1);
        state
            .transition(Input::Observation(Observation::PlatformSnapshot(
                PlatformSnapshotObservation {
                    generation: Generation(1),
                    displays: vec![display("left", 9), display("right", 10)],
                    active_display: Some(DisplayId("right".into())),
                    windows: vec![observed_window(focused, "left")],
                    focused_window: Some(focused),
                },
            )))
            .unwrap();

        let created = state
            .transition(Input::Command(Command::Workspace(WorkspaceCommand::Create {
                display: DisplayId("right".into()),
            })))
            .unwrap();

        assert_eq!(
            created
                .snapshot
                .displays
                .iter()
                .filter(|display| display.is_active_context)
                .map(|display| display.id.0.as_str())
                .collect::<Vec<_>>(),
            vec!["right"]
        );
        assert_eq!(
            created
                .snapshot
                .workspaces
                .iter()
                .filter(|workspace| workspace.display.0 == "right")
                .count(),
            2
        );
    }

    #[test]
    fn workspace_navigation_and_window_moves_are_reducer_owned() {
        let mut state = CoreState::new(CoreConfig::default());
        let focused = window(1);
        state
            .transition(Input::Observation(Observation::PlatformSnapshot(
                PlatformSnapshotObservation {
                    generation: Generation(1),
                    displays: vec![display("main", 9)],
                    active_display: Some(DisplayId("main".into())),
                    windows: vec![
                        observed_window(focused, "main"),
                        observed_window(window(2), "main"),
                    ],
                    focused_window: Some(focused),
                },
            )))
            .unwrap();
        state
            .transition(Input::Command(Command::Workspace(WorkspaceCommand::Create {
                display: DisplayId("main".into()),
            })))
            .unwrap();
        state
            .transition(Input::Command(Command::Workspace(
                WorkspaceCommand::MoveWindow {
                    workspace: number(2),
                    window: None,
                },
            )))
            .unwrap();

        let moved = state.snapshot();
        let workspace_two =
            moved.workspaces.iter().find(|workspace| workspace.number == number(2)).unwrap();
        let workspace_one =
            moved.workspaces.iter().find(|workspace| workspace.number == number(1)).unwrap();
        assert_eq!(
            moved.windows.iter().find(|window| window.id == focused).unwrap().workspace,
            Some(workspace_two.id)
        );

        // A later platform observation must not undo an explicit workspace move
        // merely because the window has no matching application rule.
        observe(&mut state, 2, vec![display("main", 9)], vec![
            observed_window(focused, "main"),
            observed_window(window(2), "main"),
        ])
        .unwrap();
        assert_eq!(
            state
                .snapshot()
                .windows
                .iter()
                .find(|window| window.id == focused)
                .unwrap()
                .workspace,
            Some(workspace_two.id)
        );

        let activated = state
            .transition(Input::Command(Command::Workspace(WorkspaceCommand::Activate(
                number(2),
            ))))
            .unwrap();
        assert!(matches!(
            activated.effects.as_slice(),
            [Effect::ApplyLayout(request)] if request.workspace == workspace_two.id
        ));
        let skip_empty = state
            .transition(Input::Command(Command::Workspace(WorkspaceCommand::Next {
                display: DisplayId("main".into()),
                skip_empty: true,
            })))
            .unwrap();
        assert!(skip_empty.events.iter().any(|event| {
            matches!(event, DomainEvent::WorkspaceChanged { workspace, .. } if *workspace == workspace_one.id)
        }));
        state
            .transition(Input::Command(Command::Workspace(WorkspaceCommand::Previous {
                display: DisplayId("main".into()),
                skip_empty: false,
            })))
            .unwrap();
        let last = state
            .transition(Input::Command(Command::Workspace(WorkspaceCommand::Last {
                display: DisplayId("main".into()),
            })))
            .unwrap();
        assert_eq!(
            last.snapshot.displays[0].active_workspace,
            Some(workspace_one.id)
        );
    }

    #[test]
    fn leaving_an_empty_workspace_destroys_it_without_losing_the_display() {
        let mut state = CoreState::new(CoreConfig::default());
        let focused = window(1);
        state
            .transition(Input::Observation(Observation::PlatformSnapshot(
                PlatformSnapshotObservation {
                    generation: Generation(1),
                    displays: vec![display("main", 9)],
                    active_display: Some(DisplayId("main".into())),
                    windows: vec![observed_window(focused, "main")],
                    focused_window: Some(focused),
                },
            )))
            .unwrap();
        state
            .transition(Input::Command(Command::Workspace(WorkspaceCommand::Create {
                display: DisplayId("main".into()),
            })))
            .unwrap();
        state
            .transition(Input::Command(Command::Workspace(WorkspaceCommand::Activate(
                number(2),
            ))))
            .unwrap();

        let returned = state
            .transition(Input::Command(Command::Workspace(WorkspaceCommand::Activate(
                number(1),
            ))))
            .unwrap();

        assert_eq!(returned.snapshot.workspaces.len(), 1);
        assert_eq!(returned.snapshot.workspaces[0].number, number(1));
        assert_eq!(returned.snapshot.displays[0].last_workspace, None);
    }

    #[test]
    fn window_interaction_commands_are_reduced_and_emit_effects() {
        let mut state = CoreState::new(CoreConfig::default());
        let first = window(1);
        let second = window(2);
        state
            .transition(Input::Observation(Observation::PlatformSnapshot(
                PlatformSnapshotObservation {
                    generation: Generation(1),
                    displays: vec![display("main", 9)],
                    active_display: Some(DisplayId("main".into())),
                    windows: vec![
                        observed_window(first, "main"),
                        observed_window(second, "main"),
                    ],
                    focused_window: Some(first),
                },
            )))
            .unwrap();

        let focused = state
            .transition(Input::Command(Command::Window(WindowCommand::Next {
                window: Some(first),
            })))
            .unwrap();
        assert_eq!(focused.snapshot.focused_window, Some(second));
        assert!(
            focused.effects.iter().any(|effect| {
                matches!(effect, Effect::FocusWindow(window) if *window == second)
            })
        );

        let before = focused.snapshot.workspaces[0].layout_frames.clone();
        let resized = state
            .transition(Input::Command(Command::Window(WindowCommand::Resize {
                amount: 0.1,
                window: Some(first),
            })))
            .unwrap();
        assert_ne!(resized.snapshot.workspaces[0].layout_frames, before);

        let fullscreen = state
            .transition(Input::Command(Command::Window(
                WindowCommand::ToggleFullscreen {
                    window: Some(first),
                    within_gaps: false,
                },
            )))
            .unwrap();
        assert_eq!(fullscreen.snapshot.workspaces[0].layout_frames.len(), 1);
    }

    #[test]
    fn focus_layer_toggle_remembers_tiled_and_floating_focus_per_workspace() {
        let mut state = CoreState::new(CoreConfig::default());
        let tiled = window(1);
        let floating = window(2);
        state
            .transition(Input::Observation(Observation::PlatformSnapshot(
                PlatformSnapshotObservation {
                    generation: Generation(1),
                    displays: vec![display("main", 9)],
                    active_display: Some(DisplayId("main".into())),
                    windows: vec![
                        observed_window(tiled, "main"),
                        observed_window(floating, "main"),
                    ],
                    focused_window: Some(tiled),
                },
            )))
            .unwrap();
        state
            .transition(Input::Command(Command::Window(WindowCommand::ToggleFloating {
                window: Some(floating),
            })))
            .unwrap();

        let to_floating = state
            .transition(Input::Command(Command::Window(
                WindowCommand::ToggleFocusLayer { window: Some(tiled) },
            )))
            .unwrap();
        assert_eq!(to_floating.snapshot.focused_window, Some(floating));

        let to_tiled = state
            .transition(Input::Command(Command::Window(
                WindowCommand::ToggleFocusLayer { window: Some(floating) },
            )))
            .unwrap();
        assert_eq!(to_tiled.snapshot.focused_window, Some(tiled));
    }

    #[test]
    fn manual_floating_and_its_visible_position_survive_platform_refreshes() {
        let mut state = CoreState::new(CoreConfig::default());
        let id = window(1);
        let initial = observed_window(id, "main");
        state
            .transition(Input::Observation(Observation::PlatformSnapshot(
                PlatformSnapshotObservation {
                    generation: Generation(1),
                    displays: vec![display("main", 9)],
                    active_display: Some(DisplayId("main".into())),
                    windows: vec![initial.clone()],
                    focused_window: Some(id),
                },
            )))
            .unwrap();
        state
            .transition(Input::Command(Command::Window(WindowCommand::ToggleFloating {
                window: Some(id),
            })))
            .unwrap();

        let mut moved = initial;
        moved.frame = Rect::new(200.0, 150.0, 500.0, 400.0).unwrap();
        let refreshed = state
            .transition(Input::Observation(Observation::PlatformSnapshot(
                PlatformSnapshotObservation {
                    generation: Generation(2),
                    displays: vec![display("main", 9)],
                    active_display: Some(DisplayId("main".into())),
                    windows: vec![moved.clone()],
                    focused_window: Some(id),
                },
            )))
            .unwrap();
        assert!(refreshed.snapshot.windows[0].floating);
        assert_eq!(refreshed.snapshot.workspaces[0].layout_frames[&id], moved.frame);
    }

    #[test]
    fn explicit_display_move_targets_that_displays_active_workspace() {
        let mut state = CoreState::new(CoreConfig::default());
        let id = window(1);
        state
            .transition(Input::Observation(Observation::PlatformSnapshot(
                PlatformSnapshotObservation {
                    generation: Generation(1),
                    displays: vec![display("left", 9), display("right", 10)],
                    active_display: Some(DisplayId("left".into())),
                    windows: vec![observed_window(id, "left")],
                    focused_window: Some(id),
                },
            )))
            .unwrap();

        let moved = state
            .transition(Input::Command(Command::Display(DisplayCommand::MoveWindowTo {
                display: DisplayId("right".into()),
                window: Some(id),
            })))
            .unwrap();

        let workspace = moved.snapshot.windows[0].workspace.unwrap();
        assert_eq!(
            moved
                .snapshot
                .workspaces
                .iter()
                .find(|candidate| candidate.id == workspace)
                .unwrap()
                .display,
            DisplayId("right".into())
        );
    }

    #[test]
    fn save_and_exit_emits_versioned_state_before_shutdown() {
        let mut state = CoreState::new(CoreConfig::default());
        state
            .transition(Input::Observation(Observation::DisplayTopology(
                DisplayTopologyObservation {
                    generation: Generation(1),
                    displays: vec![display("main", 9)],
                    active_display: Some(DisplayId("main".into())),
                },
            )))
            .unwrap();

        let transition = state.transition(Input::Command(Command::SaveAndExit)).unwrap();

        assert!(matches!(transition.effects.as_slice(), [
            Effect::Save(PersistedState { schema_version: 2, .. }),
            Effect::Shutdown(_),
            ..
        ]));
    }

    #[test]
    fn drag_swap_is_committed_by_the_core_interaction_transaction() {
        let mut state = CoreState::new(CoreConfig::default());
        let first = window(1);
        let second = window(2);
        state
            .transition(Input::Observation(Observation::PlatformSnapshot(
                PlatformSnapshotObservation {
                    generation: Generation(1),
                    displays: vec![display("main", 9)],
                    active_display: Some(DisplayId("main".into())),
                    windows: vec![
                        observed_window(first, "main"),
                        observed_window(second, "main"),
                    ],
                    focused_window: Some(first),
                },
            )))
            .unwrap();
        let before = state.snapshot().workspaces[0]
            .groups
            .iter()
            .flat_map(|group| group.windows.iter().copied())
            .collect::<Vec<_>>();
        let first_frame = state.snapshot().workspaces[0].layout_frames[&first];
        let second_frame = state.snapshot().workspaces[0].layout_frames[&second];

        let updated = state
            .transition(Input::Observation(Observation::Drag(DragObservation::Updated {
                window: first,
                frame: second_frame,
                candidates: vec![DragCandidate {
                    window: second,
                    frame: second_frame,
                }],
            })))
            .unwrap();
        assert_eq!(updated.snapshot.drag.target, Some(second));

        let committed = state
            .transition(Input::Observation(Observation::Drag(
                DragObservation::Committed { window: first },
            )))
            .unwrap();
        let after = committed.snapshot.workspaces[0]
            .groups
            .iter()
            .flat_map(|group| group.windows.iter().copied())
            .collect::<Vec<_>>();
        assert_eq!(after, before.into_iter().rev().collect::<Vec<_>>());
        assert_eq!(committed.snapshot.drag, Default::default());
        assert_eq!(
            committed.snapshot.workspaces[0].layout_frames[&first],
            second_frame
        );
        assert_eq!(
            committed.snapshot.workspaces[0].layout_frames[&second],
            first_frame
        );
    }

    #[test]
    fn mission_control_requests_and_native_observations_share_one_state_machine() {
        let mut state = CoreState::new(CoreConfig::default());
        let requested = state
            .transition(Input::Command(Command::MissionControl(
                MissionControlCommand::ShowCurrent,
            )))
            .unwrap();
        assert_eq!(
            requested.snapshot.mission_control,
            MissionControlPhase::ShowCurrentRequested
        );

        let active = state
            .transition(Input::Observation(Observation::MissionControl { active: true }))
            .unwrap();
        assert_eq!(active.snapshot.mission_control, MissionControlPhase::Active);

        let inactive = state
            .transition(Input::Observation(Observation::MissionControl { active: false }))
            .unwrap();
        assert_eq!(inactive.snapshot.mission_control, MissionControlPhase::Inactive);
    }

    #[test]
    fn directional_move_crosses_displays_by_stable_display_identity() {
        let mut state = CoreState::new(CoreConfig::default());
        let first = window(1);
        let second = window(2);
        let mut right = display("right", 10);
        right.frame = Rect::new(1200.0, 0.0, 1200.0, 800.0).unwrap();
        state
            .transition(Input::Observation(Observation::PlatformSnapshot(
                PlatformSnapshotObservation {
                    generation: Generation(1),
                    displays: vec![display("left", 9), right],
                    active_display: Some(DisplayId("left".into())),
                    windows: vec![
                        observed_window(first, "left"),
                        observed_window(second, "right"),
                    ],
                    focused_window: Some(first),
                },
            )))
            .unwrap();

        let moved = state
            .transition(Input::Command(Command::Window(WindowCommand::Move {
                direction: crate::core::command::Direction::Right,
                window: Some(first),
            })))
            .unwrap();
        let target = moved
            .snapshot
            .windows
            .iter()
            .find(|window| window.id == first)
            .and_then(|window| window.workspace)
            .unwrap();
        assert_eq!(
            moved
                .snapshot
                .workspaces
                .iter()
                .find(|workspace| workspace.id == target)
                .unwrap()
                .display,
            DisplayId("right".into())
        );
    }
}
