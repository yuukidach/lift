use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use test_log::test;

use super::testing::*;
use super::*;
use crate::model::layout::LayoutCommand;
use crate::sys::geometry::SameAs;

fn screen(x: f64) -> CGRect { CGRect::new(CGPoint::new(x, 0.0), CGSize::new(1000.0, 1000.0)) }

fn reactor_with_two_windows(
    pid: i32,
    focused: WindowId,
) -> (Apps, Reactor, actor::Receiver<raise_manager::Event>) {
    let mut apps = Apps::new();
    let mut reactor = Reactor::new_for_test();
    let (raise_tx, mut raise_rx) = actor::channel();
    reactor.communication_manager.raise_manager_tx = raise_tx;
    reactor.handle_event(screen_params_event(
        vec![screen(0.0)],
        vec![Some(SpaceId::new(1))],
        vec![],
    ));
    reactor.handle_events(apps.make_app_with_opts(pid, make_windows(2), Some(focused), true, true));
    reactor.handle_event(Event::ApplicationGloballyActivated(pid));
    apps.simulate_until_quiet(&mut reactor);
    drain_raise_requests(&mut raise_rx);
    (apps, reactor, raise_rx)
}

fn drain_raise_requests(receiver: &mut actor::Receiver<raise_manager::Event>) {
    while receiver.try_recv().is_ok() {}
}

fn take_focus_request(receiver: &mut actor::Receiver<raise_manager::Event>) -> Option<WindowId> {
    while let Ok((_, event)) = receiver.try_recv() {
        if let raise_manager::Event::RaiseRequest(request) = event
            && let Some((window, _)) = request.focus_window
        {
            return Some(window);
        }
    }
    None
}

fn take_raise_request(
    receiver: &mut actor::Receiver<raise_manager::Event>,
) -> Option<raise_manager::RaiseRequest> {
    while let Ok((_, event)) = receiver.try_recv() {
        if let raise_manager::Event::RaiseRequest(request) = event {
            return Some(request);
        }
    }
    None
}

#[test]
fn directional_focus_across_displays_is_an_authoritative_quiet_raise() {
    let mut apps = Apps::new();
    let mut reactor = Reactor::new_for_test();
    let (raise_tx, mut raise_rx) = actor::channel();
    reactor.communication_manager.raise_manager_tx = raise_tx;
    reactor.handle_event(screen_params_event(
        vec![screen(0.0), screen(1000.0)],
        vec![Some(SpaceId::new(1)), Some(SpaceId::new(2))],
        vec![],
    ));

    let target = WindowId::new(8, 1);
    let source = WindowId::new(7, 1);
    let target_window = make_window(1);
    let mut source_window = make_window(1);
    source_window.frame.origin.x += 1000.0;
    reactor.handle_events(apps.make_app_with_opts(
        target.pid,
        vec![target_window],
        Some(target),
        false,
        true,
    ));
    reactor.handle_events(apps.make_app_with_opts(
        source.pid,
        vec![source_window],
        Some(source),
        true,
        true,
    ));
    reactor.handle_event(Event::ApplicationGloballyActivated(source.pid));
    apps.simulate_until_quiet(&mut reactor);
    drain_raise_requests(&mut raise_rx);
    assert_eq!(
        reactor.core_snapshot().focused_window,
        Some(Reactor::core_window_id(source))
    );

    reactor.handle_event(Event::Command(Command::Layout(LayoutCommand::MoveFocus(
        Direction::Left,
    ))));

    let request = take_raise_request(&mut raise_rx).expect("cross-display focus request");
    assert_eq!(request.focus_window.map(|(window, _)| window), Some(target));
    assert_eq!(request.focus_quiet, Quiet::Yes);
    assert!(reactor.workspace_switch_manager.should_suppress_global_activation(target.pid));
}

#[test]
fn directional_focus_overrides_the_same_apps_activation_preference() {
    let mut apps = Apps::new();
    let mut reactor = Reactor::new_for_test();
    let (raise_tx, mut raise_rx) = actor::channel();
    reactor.communication_manager.raise_manager_tx = raise_tx;
    reactor.handle_event(screen_params_event(
        vec![screen(0.0), screen(1000.0)],
        vec![Some(SpaceId::new(1)), Some(SpaceId::new(2))],
        vec![],
    ));

    let pid = 7;
    let target = WindowId::new(pid, 1);
    let source = WindowId::new(pid, 2);
    let target_window = make_window(1);
    let mut source_window = make_window(2);
    source_window.frame.origin.x += 1000.0;
    reactor.handle_events(apps.make_app_with_opts(
        pid,
        vec![target_window, source_window],
        Some(source),
        true,
        true,
    ));
    reactor.handle_event(Event::ApplicationGloballyActivated(pid));
    apps.simulate_until_quiet(&mut reactor);

    // Recreate the short settling interval used to preserve Chrome's last
    // active window during an external app activation.
    reactor.handle_event(Event::ApplicationGloballyDeactivated(pid));
    reactor.handle_event(Event::ApplicationDeactivated(pid));
    reactor.handle_event(Event::ApplicationGloballyActivated(pid));
    reactor.handle_event(Event::ApplicationActivated(pid, Quiet::No));
    apps.simulate_until_quiet(&mut reactor);
    drain_raise_requests(&mut raise_rx);

    reactor.handle_event(Event::Command(Command::Layout(LayoutCommand::MoveFocus(
        Direction::Left,
    ))));

    let request = take_raise_request(&mut raise_rx).expect("cross-display focus request");
    assert_eq!(request.focus_window.map(|(window, _)| window), Some(target));
    reactor.handle_event(Event::ApplicationMainWindowChanged(
        pid,
        Some(target),
        request.focus_quiet,
    ));

    assert_eq!(reactor.main_window(), Some(target));
    assert_eq!(
        reactor.core_snapshot().focused_window,
        Some(Reactor::core_window_id(target))
    );
}

#[test]
fn directional_focus_target_survives_stale_main_window_from_other_display() {
    let mut apps = Apps::new();
    let mut reactor = Reactor::new_for_test();
    let (raise_tx, mut raise_rx) = actor::channel();
    reactor.communication_manager.raise_manager_tx = raise_tx;
    reactor.handle_event(screen_params_event(
        vec![screen(0.0), screen(1000.0)],
        vec![Some(SpaceId::new(1)), Some(SpaceId::new(2))],
        vec![],
    ));

    let chrome_pid = 7;
    let stale_left = WindowId::new(chrome_pid, 1);
    let target_right = WindowId::new(chrome_pid, 2);
    let mut target_window = make_window(2);
    target_window.frame.origin.x += 1000.0;
    reactor.handle_events(apps.make_app_with_opts(
        chrome_pid,
        vec![make_window(1), target_window],
        Some(stale_left),
        true,
        true,
    ));
    reactor.handle_event(Event::ApplicationGloballyActivated(chrome_pid));
    apps.simulate_until_quiet(&mut reactor);
    reactor.handle_event(Event::ApplicationGloballyDeactivated(chrome_pid));
    reactor.handle_event(Event::ApplicationDeactivated(chrome_pid));

    let codex = WindowId::new(8, 1);
    let mut codex_window = make_window(1);
    codex_window.frame.origin.x += 1000.0;
    reactor.handle_events(apps.make_app_with_opts(
        codex.pid,
        vec![codex_window],
        Some(codex),
        true,
        true,
    ));
    reactor.handle_event(Event::ApplicationGloballyActivated(codex.pid));
    apps.simulate_until_quiet(&mut reactor);
    drain_raise_requests(&mut raise_rx);

    // Model the authoritative target selected by a directional command. Chrome
    // can report its left-display AXMainWindow before activation catches up.
    reactor.handle_layout_response(
        layout::EventResponse {
            focus_window: Some(target_right),
            ..Default::default()
        },
        None,
    );
    let request = take_raise_request(&mut raise_rx).expect("directional focus request");
    assert_eq!(
        request.focus_window.map(|(window, _)| window),
        Some(target_right)
    );
    assert_eq!(request.focus_quiet, Quiet::Yes);

    reactor.handle_event(Event::ApplicationMainWindowChanged(
        chrome_pid,
        Some(stale_left),
        Quiet::No,
    ));
    reactor.handle_event(Event::ApplicationGloballyActivated(chrome_pid));
    reactor.handle_event(Event::ApplicationActivated(chrome_pid, Quiet::Yes));
    reactor.handle_event(Event::ApplicationMainWindowChanged(
        chrome_pid,
        Some(stale_left),
        Quiet::No,
    ));
    apps.simulate_until_quiet(&mut reactor);

    assert_eq!(reactor.main_window(), Some(target_right));
    assert_eq!(
        reactor.core_snapshot().focused_window,
        Some(Reactor::core_window_id(target_right))
    );
}

#[test]
fn click_to_activate_overrides_a_pending_authoritative_focus() {
    let pid = 7;
    let clicked = WindowId::new(pid, 1);
    let target = WindowId::new(pid, 2);
    let (mut apps, mut reactor, mut raise_rx) = reactor_with_two_windows(pid, clicked);

    reactor.handle_event(Event::ApplicationGloballyDeactivated(pid));
    reactor.handle_event(Event::ApplicationDeactivated(pid));
    reactor.handle_layout_response(
        layout::EventResponse {
            focus_window: Some(target),
            ..Default::default()
        },
        None,
    );
    let request = take_raise_request(&mut raise_rx).expect("authoritative focus request");
    assert_eq!(request.focus_window.map(|(window, _)| window), Some(target));

    let clicked_info = reactor
        .window_manager
        .get_window_server_info(WindowServerId::new(pid as u32 * 10_000 + 1))
        .unwrap();
    reactor.handle_event(Event::MouseDown(Some(clicked_info), CGPoint::new(50.0, 50.0)));
    reactor.handle_event(Event::ApplicationGloballyActivated(pid));
    reactor.handle_event(Event::ApplicationMainWindowChanged(
        pid,
        Some(clicked),
        Quiet::No,
    ));
    reactor.handle_event(Event::ApplicationActivated(pid, Quiet::No));
    apps.simulate_until_quiet(&mut reactor);

    assert_eq!(reactor.main_window(), Some(clicked));
    assert_eq!(
        reactor.core_snapshot().focused_window,
        Some(Reactor::core_window_id(clicked))
    );
}

#[test]
fn untracked_focus_fallback_rejects_system_surfaces() {
    let info = |layer| WindowServerInfo {
        pid: 1,
        id: WindowServerId::new(1),
        layer,
        frame: screen(0.0),
        min_frame: CGSize::ZERO,
        max_frame: CGSize::ZERO,
    };

    assert!(untracked_window_is_focusable(&info(0)));
    assert!(!untracked_window_is_focusable(&info(-1)));
    assert!(!untracked_window_is_focusable(&info(25)));
}

#[test]
fn lifecycle_events_control_activation_suppression() {
    assert_eq!(lifecycle_activation_suppression(&Event::SystemWoke), Some(true));
    assert_eq!(
        lifecycle_activation_suppression(&Event::SessionDidBecomeActive),
        Some(true)
    );
    assert_eq!(
        lifecycle_activation_suppression(&Event::MouseDown(None, CGPoint::ZERO)),
        Some(false)
    );
    assert_eq!(lifecycle_activation_suppression(&Event::MouseUp), Some(false));
    assert_eq!(
        lifecycle_activation_suppression(&Event::ApplicationGloballyActivated(7)),
        None
    );
}

#[test]
fn lifecycle_restored_activation_waits_for_user_input() {
    let mut apps = Apps::new();
    let mut reactor = Reactor::new_for_test();
    let pid = 7;
    let window = WindowId::new(pid, 1);
    let space = SpaceId::new(1);
    reactor.handle_event(screen_params_event(vec![screen(0.0)], vec![Some(space)], vec![]));
    reactor.handle_events(apps.make_app_with_opts(pid, make_windows(1), Some(window), true, true));
    apps.simulate_until_quiet(&mut reactor);
    let original = reactor.active_workspace_for_space(space).unwrap();

    reactor.handle_event(Event::Command(Command::Layout(
        LayoutCommand::MoveWindowToWorkspace {
            workspace: 1,
            window_id: Some(window.idx.get()),
        },
    )));
    let target = reactor.workspace_for_window(window).unwrap();
    assert_ne!(target, original);
    assert_eq!(reactor.active_workspace_for_space(space), Some(original));

    reactor.handle_event(Event::SessionDidBecomeActive);
    reactor.handle_event(Event::ApplicationGloballyActivated(pid));
    apps.simulate_until_quiet(&mut reactor);
    assert_eq!(
        reactor.active_workspace_for_space(space),
        Some(original),
        "lifecycle-restored activation must not switch workspaces"
    );

    reactor.handle_event(Event::MouseUp);
    assert!(!reactor.workspace_switch_manager.suppress_auto_workspace_switch_until_input);
}

#[test]
fn focus_app_rule_activates_the_new_windows_workspace() {
    let mut apps = Apps::new();
    let mut reactor = Reactor::new_for_test();
    reactor.config.virtual_workspaces =
        toml::from_str(r#"app_rules = [{ app_id = "com.testapp7", workspace = 2, focus = true }]"#)
            .unwrap();
    let core_config = crate::interfaces::config::core_config(&reactor.config).unwrap();
    reactor.window_rules = RuleSet::compile(core_config.window_rules.clone()).unwrap();
    reactor.core_state = Some(crate::core::state::CoreState::new(core_config));

    let window = WindowId::new(7, 1);
    let space = SpaceId::new(1);
    reactor.handle_event(screen_params_event(vec![screen(0.0)], vec![Some(space)], vec![]));
    reactor.handle_events(apps.make_app_with_opts(7, make_windows(1), Some(window), true, true));
    apps.simulate_until_quiet(&mut reactor);

    let workspace = reactor.workspace_for_window(window).unwrap();
    assert_eq!(reactor.active_workspace_for_space(space), Some(workspace));
    assert_eq!(reactor.workspace_number(workspace).unwrap().get(), 2);
}

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
fn layout_resize_commands_are_classified_as_interactive() {
    for command in [
        LayoutCommand::ResizeWindowGrow,
        LayoutCommand::ResizeWindowShrink,
        LayoutCommand::ResizeWindowBy { amount: 0.05 },
        LayoutCommand::ResizeWindowDirectional(Direction::Right),
    ] {
        assert!(Reactor::is_interactive_resize_event(&Event::Command(
            Command::Layout(command)
        )));
    }
    assert!(!Reactor::is_interactive_resize_event(&Event::Command(
        Command::Layout(LayoutCommand::MoveNode(Direction::Right),)
    )));
}

#[test]
fn newly_managed_windows_suppress_the_first_layout_animation() {
    let mut apps = Apps::new();
    let mut reactor = Reactor::new_for_test();
    reactor.config.settings.animate = true;
    reactor.config.settings.animation_duration = 0.2;
    let (animation_tx, mut animation_rx) = tokio::sync::mpsc::unbounded_channel();
    reactor.animation_tx = Some(animation_tx);
    reactor.handle_event(screen_params_event(
        vec![screen(0.0)],
        vec![Some(SpaceId::new(1))],
        vec![],
    ));

    let launched = apps.make_app(7, make_windows(1)).pop().unwrap();
    assert!(Reactor::should_suppress_layout_animation(&launched));
    reactor.handle_event(launched);
    assert!(matches!(
        animation_rx.try_recv(),
        Ok(animation::Message::SkipToEnd(_))
    ));

    let mut windows = make_windows(1);
    let created = Event::WindowCreated(
        WindowId::new(8, 1),
        windows.pop().unwrap(),
        None,
        Some(crate::sys::event::MouseState::Up),
    );
    assert!(Reactor::should_suppress_layout_animation(&created));

    assert!(!Reactor::should_suppress_layout_animation(&Event::MouseUp));
}

#[test]
fn perpendicular_move_node_uses_the_regular_layout_animation() {
    let mut apps = Apps::new();
    let mut reactor = Reactor::new_for_test();
    reactor.config.settings.animate = true;
    reactor.config.settings.animation_duration = 0.2;
    let (animation_tx, mut animation_rx) = tokio::sync::mpsc::unbounded_channel();
    reactor.animation_tx = Some(animation_tx);
    reactor.handle_event(screen_params_event(
        vec![screen(0.0)],
        vec![Some(SpaceId::new(1))],
        vec![],
    ));
    let focused = WindowId::new(1, 1);
    reactor.handle_events(apps.make_app_with_opts(1, make_windows(2), Some(focused), true, true));
    apps.simulate_until_quiet(&mut reactor);
    while animation_rx.try_recv().is_ok() {}

    reactor
        .transition_core_command(crate::core::command::Command::Window(
            crate::core::command::WindowCommand::ToggleOrientation {
                window: Some(Reactor::core_window_id(focused)),
            },
        ))
        .unwrap();
    assert!(reactor.update_layout_or_warn(false, false));
    assert!(matches!(
        animation_rx.try_recv(),
        Ok(animation::Message::Replace(_))
    ));
    while animation_rx.try_recv().is_ok() {}

    reactor
        .transition_core_input(crate::core::input::Input::Observation(
            crate::core::input::Observation::FocusChanged {
                window: Some(Reactor::core_window_id(focused)),
            },
        ))
        .unwrap();
    reactor
        .workspace_switch_manager
        .start_workspace_switch(WorkspaceSwitchOrigin::Manual);
    reactor.workspace_switch_manager.mark_workspace_switch_inactive();
    assert!(reactor.workspace_switch_manager.active_workspace_switch.is_some());

    reactor.handle_event(Event::Command(Command::Layout(LayoutCommand::MoveNode(
        Direction::Right,
    ))));
    assert!(reactor.workspace_switch_manager.active_workspace_switch.is_none());
    assert!(matches!(
        animation_rx.try_recv(),
        Ok(animation::Message::Replace(_))
    ));
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

    let (_, event) = broadcasts.try_recv().expect("layout_changed broadcast");
    let BroadcastEvent::LayoutChanged { layout, .. } = event else {
        panic!("expected layout_changed broadcast");
    };
    assert_eq!(layout.mode, "bsp");
    assert_eq!(layout.tiled_windows.len(), 2);

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
fn destroying_the_focused_main_window_reflows_and_refocuses_the_remaining_window() {
    let pid = 7;
    let destroyed = WindowId::new(pid, 1);
    let remaining = WindowId::new(pid, 2);
    let (mut apps, mut reactor, mut raise_rx) = reactor_with_two_windows(pid, destroyed);
    let before = apps.windows[&remaining].frame;

    reactor.handle_event(Event::WindowDestroyed(destroyed));
    apps.simulate_until_quiet(&mut reactor);

    let after = apps.windows[&remaining].frame;
    assert_eq!(reactor.core_snapshot().windows.len(), 1);
    assert_eq!(take_focus_request(&mut raise_rx), Some(remaining));
    assert!(after.size.width > before.size.width);
    assert!(after.size.height >= before.size.height);
}

#[test]
fn minimizing_the_focused_fullscreen_window_refocuses_the_remaining_window() {
    let pid = 7;
    let minimized = WindowId::new(pid, 1);
    let remaining = WindowId::new(pid, 2);
    let (mut apps, mut reactor, mut raise_rx) = reactor_with_two_windows(pid, minimized);

    reactor.handle_event(Event::Command(Command::Layout(LayoutCommand::ToggleFullscreen)));
    apps.simulate_until_quiet(&mut reactor);
    drain_raise_requests(&mut raise_rx);
    let before = apps.windows[&remaining].frame;

    reactor.handle_event(Event::WindowMinimized(minimized));
    apps.simulate_until_quiet(&mut reactor);

    let after = apps.windows[&remaining].frame;
    assert_eq!(take_focus_request(&mut raise_rx), Some(remaining));
    assert!(after.size.width > before.size.width);
    assert!(after.size.height >= before.size.height);

    reactor.handle_event(Event::ApplicationMainWindowChanged(
        pid,
        Some(remaining),
        Quiet::No,
    ));
    apps.simulate_until_quiet(&mut reactor);
    drain_raise_requests(&mut raise_rx);
    reactor.handle_event(Event::WindowDeminiaturized(minimized));
    apps.simulate_until_quiet(&mut reactor);

    assert_eq!(
        take_focus_request(&mut raise_rx),
        None,
        "restoring a minimized window must not steal focus"
    );
}

#[test]
fn minimizing_an_unfocused_window_does_not_request_refocus() {
    let pid = 7;
    let focused = WindowId::new(pid, 1);
    let minimized = WindowId::new(pid, 2);
    let (mut apps, mut reactor, mut raise_rx) = reactor_with_two_windows(pid, focused);

    reactor.handle_event(Event::WindowMinimized(minimized));
    apps.simulate_until_quiet(&mut reactor);

    assert_eq!(take_focus_request(&mut raise_rx), None);
    assert_eq!(
        reactor.core_snapshot().focused_window,
        Some(Reactor::core_window_id(focused))
    );
}

#[test]
fn focus_triggered_tiled_resize_is_restored_without_moving_neighbors() {
    let mut apps = Apps::new();
    let mut reactor = Reactor::new_for_test();
    let first = WindowId::new(7, 1);
    let second = WindowId::new(7, 2);
    reactor.handle_event(screen_params_event(
        vec![screen(0.0)],
        vec![Some(SpaceId::new(1))],
        vec![],
    ));
    reactor.handle_events(apps.make_app_with_opts(7, make_windows(2), Some(first), true, true));
    apps.simulate_until_quiet(&mut reactor);
    let first_tiled = apps.windows[&first].frame;
    let second_tiled = apps.windows[&second].frame;

    reactor.send_layout_event(LayoutEvent::WindowFocused(first));
    let self_resized = CGRect::new(
        first_tiled.origin,
        CGSize::new(first_tiled.size.width + 2.0, first_tiled.size.height + 1.0),
    );
    apps.windows.get_mut(&first).unwrap().frame = self_resized;
    reactor.handle_event(Event::WindowFrameChanged(
        first,
        self_resized,
        None,
        Requested(false),
        Some(crate::sys::event::MouseState::Up),
    ));
    apps.simulate_until_quiet(&mut reactor);

    assert_eq!(apps.windows[&first].frame, first_tiled);
    assert_eq!(apps.windows[&second].frame, second_tiled);
}

#[test]
fn mouse_dragged_resize_persists_after_mouse_up() {
    let mut apps = Apps::new();
    let mut reactor = Reactor::new_for_test();
    let first = WindowId::new(7, 1);
    let second = WindowId::new(7, 2);
    reactor.handle_event(screen_params_event(
        vec![screen(0.0)],
        vec![Some(SpaceId::new(1))],
        vec![],
    ));
    reactor.handle_events(apps.make_app_with_opts(7, make_windows(2), Some(second), true, true));
    apps.simulate_until_quiet(&mut reactor);
    let first_tiled = apps.windows[&first].frame;
    let second_tiled = apps.windows[&second].frame;
    let dragged = CGRect::new(
        CGPoint::new(second_tiled.origin.x - 200.0, second_tiled.origin.y),
        CGSize::new(second_tiled.size.width + 200.0, second_tiled.size.height),
    );
    apps.windows.get_mut(&second).unwrap().frame = dragged;

    reactor.handle_event(Event::WindowFrameChanged(
        second,
        dragged,
        None,
        Requested(false),
        Some(crate::sys::event::MouseState::Down),
    ));
    reactor.handle_event(Event::MouseUp);
    apps.simulate_until_quiet(&mut reactor);

    assert!(apps.windows[&second].frame.size.width > second_tiled.size.width);
    assert!(apps.windows[&first].frame.size.width < first_tiled.size.width);
    assert!(apps.windows[&second].frame.same_as(dragged));
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
        .find(|workspace| workspace.number.is_some_and(|number| number.get() == 2))
        .unwrap();
    assert_eq!(reactor.workspace_for_window(window), Some(target.id));
}

#[test]
fn frontmost_main_window_change_without_auxiliary_click_does_not_switch_workspaces() {
    let mut apps = Apps::new();
    let mut reactor = Reactor::new_for_test();
    let pid = 7;
    let first = WindowId::new(pid, 1);
    let expanded = WindowId::new(pid, 2);
    let space = SpaceId::new(1);
    reactor.handle_event(screen_params_event(vec![screen(0.0)], vec![Some(space)], vec![]));
    reactor.handle_events(apps.make_app_with_opts(pid, make_windows(2), Some(first), true, true));
    reactor.handle_event(Event::ApplicationGloballyActivated(pid));
    apps.simulate_until_quiet(&mut reactor);
    let original = reactor.active_workspace_for_space(space).unwrap();

    reactor.handle_event(Event::Command(Command::Layout(
        LayoutCommand::MoveWindowToWorkspace {
            workspace: 1,
            window_id: Some(expanded.idx.get()),
        },
    )));
    let target = reactor.workspace_for_window(expanded).unwrap();
    assert_ne!(target, original);
    assert_eq!(reactor.active_workspace_for_space(space), Some(original));

    reactor.handle_event(Event::ApplicationMainWindowChanged(
        pid,
        Some(expanded),
        Quiet::No,
    ));
    apps.simulate_until_quiet(&mut reactor);

    assert_eq!(reactor.active_workspace_for_space(space), Some(original));
    assert_eq!(reactor.workspace_for_window(expanded), Some(target));
    assert_eq!(
        reactor.core_snapshot().focused_window,
        Some(Reactor::core_window_id(expanded))
    );
}

#[test]
fn external_reactivation_keeps_the_apps_last_used_window() {
    let mut apps = Apps::new();
    let mut reactor = Reactor::new_for_test();
    let pid = 7;
    let first = WindowId::new(pid, 1);
    let last_used = WindowId::new(pid, 2);
    let space = SpaceId::new(1);
    reactor.handle_event(screen_params_event(vec![screen(0.0)], vec![Some(space)], vec![]));
    reactor.handle_events(apps.make_app_with_opts(pid, make_windows(2), Some(first), true, true));
    reactor.handle_event(Event::ApplicationGloballyActivated(pid));
    apps.simulate_until_quiet(&mut reactor);

    let last_used_info = reactor
        .window_manager
        .get_window_server_info(WindowServerId::new(pid as u32 * 10_000 + 2))
        .unwrap();
    reactor.handle_event(Event::MouseDown(
        Some(last_used_info),
        CGPoint::new(150.0, 150.0),
    ));
    reactor.handle_event(Event::ApplicationMainWindowChanged(
        pid,
        Some(last_used),
        Quiet::No,
    ));
    assert_eq!(
        reactor.core_snapshot().focused_window,
        Some(Reactor::core_window_id(last_used))
    );

    reactor.handle_event(Event::ApplicationGloballyDeactivated(pid));
    reactor.handle_event(Event::ApplicationDeactivated(pid));

    // Chrome can expose an older AXMainWindow before it dispatches an external
    // URL to its own last-active browser window. That transient value must not
    // replace the window that was active when the app deactivated.
    reactor.handle_event(Event::ApplicationMainWindowChanged(pid, Some(first), Quiet::No));
    reactor.handle_event(Event::ApplicationGloballyActivated(pid));
    reactor.handle_event(Event::ApplicationActivated(pid, Quiet::No));
    apps.simulate_until_quiet(&mut reactor);

    assert_eq!(
        reactor.core_snapshot().focused_window,
        Some(Reactor::core_window_id(last_used))
    );
    assert_eq!(reactor.main_window(), Some(last_used));
}

#[test]
fn quiet_workspace_raise_becomes_the_apps_last_used_window() {
    let mut apps = Apps::new();
    let mut reactor = Reactor::new_for_test();
    let pid = 7;
    let previous = WindowId::new(pid, 1);
    let workspace_target = WindowId::new(pid, 2);
    let space = SpaceId::new(1);
    reactor.handle_event(screen_params_event(vec![screen(0.0)], vec![Some(space)], vec![]));
    reactor.handle_events(apps.make_app_with_opts(
        pid,
        make_windows(2),
        Some(previous),
        true,
        true,
    ));
    reactor.handle_event(Event::ApplicationGloballyActivated(pid));
    apps.simulate_until_quiet(&mut reactor);

    reactor.handle_event(Event::ApplicationGloballyDeactivated(pid));
    reactor.handle_event(Event::ApplicationDeactivated(pid));
    reactor.handle_event(Event::ApplicationGloballyActivated(pid));
    reactor.handle_event(Event::ApplicationActivated(pid, Quiet::No));

    // Workspace focus raises are quiet so they do not auto-switch workspaces,
    // but they still represent an explicit last-used window selection.
    reactor.handle_event(Event::ApplicationMainWindowChanged(
        pid,
        Some(workspace_target),
        Quiet::Yes,
    ));
    assert_eq!(reactor.main_window(), Some(workspace_target));

    reactor.handle_event(Event::ApplicationGloballyDeactivated(pid));
    reactor.handle_event(Event::ApplicationDeactivated(pid));

    // Chrome may expose the previous AXMainWindow before dispatching an
    // external URL. Reactivation must preserve the workspace-selected window.
    reactor.handle_event(Event::ApplicationMainWindowChanged(
        pid,
        Some(previous),
        Quiet::No,
    ));
    reactor.handle_event(Event::ApplicationGloballyActivated(pid));
    reactor.handle_event(Event::ApplicationActivated(pid, Quiet::No));
    apps.simulate_until_quiet(&mut reactor);

    assert_eq!(reactor.main_window(), Some(workspace_target));
}

#[test]
fn direct_click_overrides_the_pre_deactivation_window() {
    let mut apps = Apps::new();
    let mut reactor = Reactor::new_for_test();
    let pid = 7;
    let first = WindowId::new(pid, 1);
    let previous = WindowId::new(pid, 2);
    let space = SpaceId::new(1);
    reactor.handle_event(screen_params_event(vec![screen(0.0)], vec![Some(space)], vec![]));
    reactor.handle_events(apps.make_app_with_opts(
        pid,
        make_windows(2),
        Some(previous),
        true,
        true,
    ));
    reactor.handle_event(Event::ApplicationGloballyActivated(pid));
    apps.simulate_until_quiet(&mut reactor);
    reactor.handle_event(Event::ApplicationGloballyDeactivated(pid));
    reactor.handle_event(Event::ApplicationDeactivated(pid));

    let clicked_info = reactor
        .window_manager
        .get_window_server_info(WindowServerId::new(pid as u32 * 10_000 + 1))
        .unwrap();
    reactor.handle_event(Event::MouseDown(Some(clicked_info), CGPoint::new(50.0, 50.0)));
    reactor.handle_event(Event::ApplicationMainWindowChanged(pid, Some(first), Quiet::No));
    reactor.handle_event(Event::ApplicationGloballyActivated(pid));
    reactor.handle_event(Event::ApplicationActivated(pid, Quiet::No));
    apps.simulate_until_quiet(&mut reactor);

    assert_eq!(
        reactor.core_snapshot().focused_window,
        Some(Reactor::core_window_id(first))
    );
    assert_eq!(reactor.main_window(), Some(first));
}

#[test]
fn quiet_main_window_change_does_not_switch_workspaces() {
    let mut apps = Apps::new();
    let mut reactor = Reactor::new_for_test();
    let pid = 7;
    let first = WindowId::new(pid, 1);
    let background = WindowId::new(pid, 2);
    let space = SpaceId::new(1);
    reactor.handle_event(screen_params_event(vec![screen(0.0)], vec![Some(space)], vec![]));
    reactor.handle_events(apps.make_app_with_opts(pid, make_windows(2), Some(first), true, true));
    reactor.handle_event(Event::ApplicationGloballyActivated(pid));
    apps.simulate_until_quiet(&mut reactor);
    let original = reactor.active_workspace_for_space(space).unwrap();

    reactor.handle_event(Event::Command(Command::Layout(
        LayoutCommand::MoveWindowToWorkspace {
            workspace: 1,
            window_id: Some(background.idx.get()),
        },
    )));
    assert_ne!(reactor.workspace_for_window(background), Some(original));

    reactor.handle_event(Event::ApplicationMainWindowChanged(
        pid,
        Some(background),
        Quiet::Yes,
    ));
    apps.simulate_until_quiet(&mut reactor);

    assert_eq!(reactor.active_workspace_for_space(space), Some(original));
}

#[test]
fn inactive_manageable_window_allows_unmanageable_controller_activation() {
    let mut apps = Apps::new();
    let mut reactor = Reactor::new_for_test();
    let pid = 7;
    let window = WindowId::new(pid, 1);
    let space = SpaceId::new(1);
    reactor.handle_event(screen_params_event(vec![screen(0.0)], vec![Some(space)], vec![]));
    reactor.handle_events(apps.make_app_with_opts(pid, make_windows(1), Some(window), true, true));
    apps.simulate_until_quiet(&mut reactor);

    reactor.handle_event(Event::Command(Command::Layout(
        LayoutCommand::MoveWindowToWorkspace {
            workspace: 1,
            window_id: Some(window.idx.get()),
        },
    )));
    let target = reactor.workspace_for_window(window).unwrap();
    assert_ne!(reactor.active_workspace_for_space(space), Some(target));
    assert!(reactor.has_manageable_window_on_inactive_workspace(pid));

    reactor.handle_event(Event::Command(Command::Layout(
        LayoutCommand::SwitchToWorkspace(1),
    )));

    assert_eq!(reactor.active_workspace_for_space(space), Some(target));
    assert!(!reactor.has_manageable_window_on_inactive_workspace(pid));
}

#[test]
fn auxiliary_window_global_activation_moves_main_window_to_click_workspace() {
    let mut apps = Apps::new();
    let mut reactor = Reactor::new_for_test();
    let pid = 7;
    let window = WindowId::new(pid, 1);
    let space = SpaceId::new(1);
    let mut controller = make_window(2);
    controller.is_standard = false;
    reactor.handle_event(screen_params_event(vec![screen(0.0)], vec![Some(space)], vec![]));
    reactor.handle_events(apps.make_app_with_opts(
        pid,
        vec![make_window(1), controller],
        Some(window),
        true,
        true,
    ));
    apps.simulate_until_quiet(&mut reactor);

    reactor.handle_event(Event::Command(Command::Layout(
        LayoutCommand::MoveWindowToWorkspace {
            workspace: 1,
            window_id: Some(window.idx.get()),
        },
    )));
    let target = reactor.workspace_for_window(window).unwrap();
    let origin = reactor.active_workspace_for_space(space).unwrap();
    assert_ne!(origin, target);

    let controller_wsid = WindowServerId::new(pid as u32 * 10_000 + 2);
    let controller_info = reactor.window_manager.get_window_server_info(controller_wsid).unwrap();
    reactor.handle_event(Event::MouseDown(Some(controller_info), CGPoint::new(10.0, 10.0)));
    reactor.handle_event(Event::ApplicationGloballyActivated(pid));
    apps.simulate_until_quiet(&mut reactor);

    assert_eq!(reactor.active_workspace_for_space(space), Some(origin));
    assert_eq!(reactor.workspace_for_window(window), Some(origin));
}

#[test]
fn auxiliary_window_activation_moves_main_window_without_main_window_change() {
    let mut apps = Apps::new();
    let mut reactor = Reactor::new_for_test();
    let pid = 7;
    let window = WindowId::new(pid, 1);
    let space = SpaceId::new(1);
    reactor.handle_event(screen_params_event(vec![screen(0.0)], vec![Some(space)], vec![]));
    reactor.handle_events(apps.make_app_with_opts(pid, make_windows(1), Some(window), true, true));
    apps.simulate_until_quiet(&mut reactor);

    reactor.handle_event(Event::Command(Command::Layout(
        LayoutCommand::MoveWindowToWorkspace {
            workspace: 1,
            window_id: Some(window.idx.get()),
        },
    )));
    let hidden = reactor.workspace_for_window(window).unwrap();
    let origin = reactor.active_workspace_for_space(space).unwrap();
    assert_ne!(origin, hidden);

    let controller_info = WindowServerInfo {
        pid,
        id: WindowServerId::new(99_999),
        layer: 3,
        frame: CGRect::new(CGPoint::new(700.0, 50.0), CGSize::new(250.0, 180.0)),
        min_frame: CGSize::ZERO,
        max_frame: CGSize::ZERO,
    };
    reactor.handle_event(Event::MouseDown(
        Some(controller_info),
        CGPoint::new(800.0, 100.0),
    ));
    reactor.handle_event(Event::ApplicationActivated(pid, Quiet::No));
    apps.simulate_until_quiet(&mut reactor);

    assert_eq!(reactor.active_workspace_for_space(space), Some(origin));
    assert_eq!(reactor.workspace_for_window(window), Some(origin));
}

#[test]
fn elevated_untracked_surface_without_hidden_main_window_is_ignored() {
    let mut apps = Apps::new();
    let mut reactor = Reactor::new_for_test();
    let pid = 7;
    let window = WindowId::new(pid, 1);
    let space = SpaceId::new(1);
    reactor.handle_event(screen_params_event(vec![screen(0.0)], vec![Some(space)], vec![]));
    reactor.handle_events(apps.make_app_with_opts(pid, make_windows(1), Some(window), true, true));
    apps.simulate_until_quiet(&mut reactor);

    let controller_wsid = WindowServerId::new(99_999);
    let controller_info = WindowServerInfo {
        pid,
        id: controller_wsid,
        layer: 3,
        frame: CGRect::new(CGPoint::new(700.0, 50.0), CGSize::new(250.0, 180.0)),
        min_frame: CGSize::ZERO,
        max_frame: CGSize::ZERO,
    };
    reactor.update_partial_window_server_info(vec![controller_info]);
    reactor.handle_event(Event::MouseDown(
        Some(controller_info),
        CGPoint::new(800.0, 100.0),
    ));

    assert!(reactor.refocus_manager.auxiliary_window_workspace_target.is_none());
}

#[test]
fn hidden_workspace_move_command_moves_without_switching() {
    let mut apps = Apps::new();
    let mut reactor = Reactor::new_for_test();
    let space = SpaceId::new(1);
    reactor.handle_event(screen_params_event(vec![screen(0.0)], vec![Some(space)], vec![]));
    let window = WindowId::new(7, 1);
    reactor.handle_events(apps.make_app_with_opts(7, make_windows(1), Some(window), true, true));
    apps.simulate_until_quiet(&mut reactor);
    let regular = reactor.active_workspace_for_space(space).unwrap();

    reactor.handle_event(Event::Command(Command::Layout(
        LayoutCommand::MoveWindowToHiddenWorkspace { window_id: None },
    )));

    let hidden = reactor.workspace_for_window(window).unwrap();
    assert_ne!(hidden, regular);
    assert_eq!(reactor.active_workspace_for_space(space), Some(regular));
    assert_eq!(
        reactor
            .core_snapshot()
            .workspaces
            .iter()
            .find(|workspace| workspace.id == hidden)
            .unwrap()
            .number,
        None
    );

    reactor.handle_event(Event::Command(Command::Layout(
        LayoutCommand::ToggleHiddenWorkspace,
    )));
    assert_eq!(reactor.active_workspace_for_space(space), Some(hidden));
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
        .find(|workspace| workspace.number.is_some_and(|number| number.get() == 2))
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
        .find(|workspace| workspace.number.is_some_and(|number| number.get() == 8))
        .unwrap();
    assert_eq!(reactor.active_workspace_for_space(space), Some(target.id));

    reactor.handle_event(Event::Command(Command::Layout(
        LayoutCommand::SwitchToGlobalSlot(9),
    )));
    let snapshot = reactor.core_snapshot();
    let zero = snapshot
        .workspaces
        .iter()
        .find(|workspace| workspace.number.is_some_and(|number| number.get() == 0))
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
