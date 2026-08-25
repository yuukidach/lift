use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use test_log::test;

use super::testing::*;
use super::*;
use crate::model::layout::LayoutCommand;

fn screen(x: f64) -> CGRect { CGRect::new(CGPoint::new(x, 0.0), CGSize::new(1000.0, 1000.0)) }

#[test]
fn publishes_stable_immutable_core_snapshots() {
    let mut apps = Apps::new();
    let mut reactor = Reactor::new_for_test();
    reactor.handle_event(screen_params_event(
        vec![screen(0.0)],
        vec![Some(SpaceId::new(1))],
        vec![],
    ));
    reactor.handle_events(apps.make_app(1, make_windows(2)));
    apps.simulate_until_quiet(&mut reactor);

    reactor.publish_core_snapshot().unwrap();
    let first = reactor.snapshot_store.load();
    assert_eq!(first.displays.len(), 1);
    assert_eq!(first.workspaces.len(), 1);
    assert_eq!(first.windows.len(), 2);
    assert!(
        first
            .windows
            .iter()
            .all(|window| window.workspace == Some(first.workspaces[0].id))
    );

    reactor.publish_core_snapshot().unwrap();
    let second = reactor.snapshot_store.load();
    assert_eq!(second.revision, first.revision + 1);
    assert_eq!(second.workspaces[0].id, first.workspaces[0].id);
    assert_eq!(first.revision, 1, "existing readers retain their snapshot");
}

#[test]
fn publishing_window_membership_changes_broadcasts_once_for_the_display() {
    let mut apps = Apps::new();
    let (mut reactor, mut broadcasts) = Reactor::new_for_test_with_broadcast();
    reactor.handle_event(screen_params_event(
        vec![screen(0.0)],
        vec![Some(SpaceId::new(1))],
        vec![],
    ));
    reactor.publish_core_snapshot().unwrap();
    while broadcasts.try_recv().is_ok() {}

    reactor.handle_events(apps.make_app(1, make_windows(2)));
    apps.simulate_until_quiet(&mut reactor);
    reactor.publish_core_snapshot().unwrap();

    let (_, event) = broadcasts.try_recv().expect("windows_changed broadcast");
    let BroadcastEvent::WindowsChanged {
        windows,
        space_id,
        display_uuid,
        ..
    } = event
    else {
        panic!("expected windows_changed broadcast");
    };
    assert_eq!(windows.len(), 2);
    assert_eq!(space_id, SpaceId::new(1));
    assert_eq!(display_uuid.as_deref(), Some("test-display-0"));

    reactor.publish_core_snapshot().unwrap();
    assert!(
        broadcasts.try_recv().is_err(),
        "an unchanged snapshot must not trigger another update"
    );
}

#[test]
fn destroyed_window_is_removed_before_the_same_event_reflows_layout() {
    let mut apps = Apps::new();
    let mut reactor = Reactor::new_for_test();
    reactor.handle_event(screen_params_event(
        vec![screen(0.0)],
        vec![Some(SpaceId::new(1))],
        vec![],
    ));
    reactor.handle_events(apps.make_app(1, make_windows(2)));
    apps.simulate_until_quiet(&mut reactor);
    assert_eq!(reactor.core_snapshot().windows.len(), 2);

    reactor.handle_event(Event::WindowDestroyed(WindowId::new(1, 1)));

    assert_eq!(reactor.window_manager.tracked_window_count(), 1);
    assert_eq!(reactor.core_snapshot().windows.len(), 1);
    assert_eq!(reactor.core_snapshot().windows[0].id.index.get(), 2);
}

#[test]
fn terminated_application_removes_all_windows_before_reflow() {
    let mut apps = Apps::new();
    let mut reactor = Reactor::new_for_test();
    reactor.handle_event(screen_params_event(
        vec![screen(0.0)],
        vec![Some(SpaceId::new(1))],
        vec![],
    ));
    reactor.handle_events(apps.make_app(7, make_windows(2)));
    apps.simulate_until_quiet(&mut reactor);
    assert_eq!(reactor.core_snapshot().windows.len(), 2);

    reactor.handle_event(Event::ApplicationThreadTerminated(7));

    assert_eq!(reactor.window_manager.tracked_window_count(), 0);
    assert!(reactor.core_snapshot().windows.is_empty());
}

#[test]
fn workspace_move_command_changes_the_authoritative_assignment() {
    let mut apps = Apps::new();
    let mut reactor = Reactor::new_for_test();
    let space = SpaceId::new(1);
    reactor.handle_event(screen_params_event(vec![screen(0.0)], vec![Some(space)], vec![]));
    let window = WindowId::new(7, 1);
    reactor.handle_events(apps.make_app_with_opts(7, make_windows(1), Some(window), true, true));
    apps.simulate_until_quiet(&mut reactor);

    reactor.handle_event(Event::Command(Command::Layout(
        LayoutCommand::MoveWindowToWorkspace { workspace: 1, window_id: None },
    )));

    let snapshot = reactor.core_snapshot();
    let target = snapshot
        .workspaces
        .iter()
        .find(|workspace| workspace.number.get() == 2)
        .unwrap();
    assert_eq!(reactor.workspace_for_window(window), Some(target.id));
}

#[test]
fn cross_display_workspace_move_survives_the_next_platform_observation() {
    let mut apps = Apps::new();
    let mut reactor = Reactor::new_for_test();
    reactor.handle_event(screen_params_event(
        vec![screen(0.0), screen(1000.0)],
        vec![Some(SpaceId::new(1)), Some(SpaceId::new(2))],
        vec![],
    ));
    let window = WindowId::new(7, 1);
    reactor.handle_events(apps.make_app_with_opts(7, make_windows(1), Some(window), true, true));
    apps.simulate_until_quiet(&mut reactor);

    reactor.handle_event(Event::Command(Command::Layout(
        LayoutCommand::MoveWindowToWorkspace { workspace: 1, window_id: None },
    )));
    let target = reactor
        .core_snapshot()
        .workspaces
        .iter()
        .find(|workspace| workspace.number.get() == 2)
        .unwrap()
        .id;
    assert_eq!(reactor.workspace_for_window(window), Some(target));

    // The synthetic AX frame still overlaps the source display here, exactly
    // like production before the asynchronous cross-display write completes.
    reactor.advance_core_state().unwrap();
    assert_eq!(reactor.workspace_for_window(window), Some(target));
}

#[test]
fn global_slot_command_creates_and_activates_the_requested_workspace() {
    let mut reactor = Reactor::new_for_test();
    let space = SpaceId::new(1);
    reactor.handle_event(screen_params_event(vec![screen(0.0)], vec![Some(space)], vec![]));

    reactor.handle_event(Event::Command(Command::Layout(
        LayoutCommand::SwitchToGlobalSlot(7),
    )));

    let snapshot = reactor.core_snapshot();
    let target = snapshot
        .workspaces
        .iter()
        .find(|workspace| workspace.number.get() == 8)
        .unwrap();
    assert_eq!(reactor.active_workspace_for_space(space), Some(target.id));

    reactor.handle_event(Event::Command(Command::Layout(
        LayoutCommand::SwitchToGlobalSlot(9),
    )));
    let snapshot = reactor.core_snapshot();
    let zero = snapshot
        .workspaces
        .iter()
        .find(|workspace| workspace.number.get() == 0)
        .unwrap();
    assert_eq!(zero.name, "Workspace 0");
    assert_eq!(reactor.active_workspace_for_space(space), Some(zero.id));
}

#[test]
fn removing_a_display_migrates_workspaces_by_display_identity() {
    let mut reactor = Reactor::new_for_test();
    reactor.handle_event(screen_params_event(
        vec![screen(0.0), screen(1000.0)],
        vec![Some(SpaceId::new(1)), Some(SpaceId::new(2))],
        vec![],
    ));
    let before = reactor.core_snapshot();
    assert_eq!(before.workspaces.len(), 2);

    reactor.space_manager.screens =
        make_screen_snapshots(vec![screen(0.0)], vec![Some(SpaceId::new(1))]);
    reactor.prepare_core_topology_transition().unwrap();
    let after = reactor.core_snapshot();
    assert_eq!(after.workspaces.len(), 2);
    assert!(after.workspaces.iter().all(|workspace| workspace.display.0 == "test-display-0"));
}
