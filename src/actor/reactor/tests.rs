use objc2_core_foundation::{CGPoint, CGSize};
use test_log::test;

use super::display_topology::TopologyState;
use super::testing::*;
use super::*;
use crate::actor::app::{Request, pid_t};
use crate::layout_engine::{Direction, LayoutCommand, LayoutEngine, LayoutEvent};
use crate::sys::app::WindowInfo;
use crate::sys::window_server::WindowServerId;

#[test]
fn it_ignores_stale_resize_events() {
    let mut apps = Apps::new();
    let mut reactor = Reactor::new_for_test(LayoutEngine::new(
        &crate::common::config::VirtualWorkspaceSettings::default(),
        &crate::common::config::LayoutSettings::default(),
        None,
    ));
    reactor.handle_event(screen_params_event(
        vec![CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.))],
        vec![Some(SpaceId::new(1))],
        vec![],
    ));

    reactor.handle_events(apps.make_app(1, make_windows(2)));
    let requests = apps.requests();
    assert!(!requests.is_empty());
    let events_1 = apps.simulate_events_for_requests(requests);

    reactor.handle_events(apps.make_app(2, make_windows(2)));
    assert!(!apps.requests().is_empty());

    for event in dbg!(events_1) {
        reactor.handle_event(event);
    }
    let requests = apps.requests();
    assert!(
        requests.is_empty(),
        "got requests when there should have been none: {requests:?}"
    );
}

#[test]
fn it_sends_writes_when_stale_read_state_looks_same_as_written_state() {
    let mut apps = Apps::new();
    let mut reactor = Reactor::new_for_test(LayoutEngine::new(
        &crate::common::config::VirtualWorkspaceSettings::default(),
        &crate::common::config::LayoutSettings::default(),
        None,
    ));
    reactor.handle_event(screen_params_event(
        vec![CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.))],
        vec![Some(SpaceId::new(1))],
        vec![],
    ));

    reactor.handle_events(apps.make_app(1, make_windows(2)));
    let events_1 = apps.simulate_events();
    let state_1 = apps.windows.clone();
    assert!(!state_1.is_empty());

    for event in events_1 {
        reactor.handle_event(event);
    }
    assert!(apps.requests().is_empty());

    reactor.handle_events(apps.make_app(2, make_windows(1)));
    let _events_2 = apps.simulate_events();

    reactor.handle_event(Event::WindowDestroyed(WindowId::new(2, 1)));
    let _events_3 = apps.simulate_events();
    let state_3 = apps.windows;

    // These should be the same, because we should have resized the first
    // two windows both at the beginning, and at the end when the third
    // window was destroyed.
    for (wid, state) in dbg!(state_1) {
        assert!(state_3.contains_key(&wid), "{wid:?} not in {state_3:#?}");
        assert_eq!(state.frame, state_3[&wid].frame);
    }
}

#[test]
fn it_manages_windows_on_enabled_spaces() {
    let mut apps = Apps::new();
    let mut reactor = Reactor::new_for_test(LayoutEngine::new(
        &crate::common::config::VirtualWorkspaceSettings::default(),
        &crate::common::config::LayoutSettings::default(),
        None,
    ));
    let full_screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    reactor.handle_event(screen_params_event(
        vec![full_screen],
        vec![Some(SpaceId::new(1))],
        vec![],
    ));

    reactor.handle_events(apps.make_app(1, make_windows(1)));

    let _events = apps.simulate_events();
    assert_eq!(
        full_screen,
        apps.windows.get(&WindowId::new(1, 1)).expect("Window was not resized").frame,
    );
}

#[test]
fn it_clears_screen_state_when_no_displays_are_reported() {
    let mut reactor = Reactor::new_for_test(LayoutEngine::new(
        &crate::common::config::VirtualWorkspaceSettings::default(),
        &crate::common::config::LayoutSettings::default(),
        None,
    ));
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));

    reactor.handle_event(screen_params_event(
        vec![screen],
        vec![Some(SpaceId::new(1))],
        vec![],
    ));
    assert_eq!(1, reactor.space_manager.screens.len());

    reactor.handle_event(screen_params_event(vec![], vec![], vec![]));
    assert!(reactor.space_manager.screens.is_empty());

    reactor.handle_event(Event::SpaceChanged(vec![]));
    assert!(reactor.space_manager.screens.is_empty());

    reactor.handle_event(screen_params_event(
        vec![screen],
        vec![Some(SpaceId::new(1))],
        vec![],
    ));
    assert_eq!(1, reactor.space_manager.screens.len());
}

#[test]
fn duplicate_space_changed_snapshot_is_ignored() {
    let mut apps = Apps::new();
    let mut reactor = Reactor::new_for_test(LayoutEngine::new(
        &crate::common::config::VirtualWorkspaceSettings::default(),
        &crate::common::config::LayoutSettings::default(),
        None,
    ));
    let frame = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let space = SpaceId::new(1);

    reactor.handle_event(screen_params_event(vec![frame], vec![Some(space)], vec![]));
    reactor.handle_events(apps.make_app(1, make_windows(1)));
    apps.simulate_until_quiet(&mut reactor);
    let _ = apps.requests();

    reactor.handle_event(Event::SpaceChanged(vec![Some(space)]));
    let requests = apps.requests();
    assert!(
        requests.is_empty(),
        "duplicate SpaceChanged should not trigger refresh requests: {requests:?}"
    );
}

#[test]
fn it_ignores_windows_on_disabled_spaces() {
    let mut apps = Apps::new();
    let mut reactor = Reactor::new_for_test(LayoutEngine::new(
        &crate::common::config::VirtualWorkspaceSettings::default(),
        &crate::common::config::LayoutSettings::default(),
        None,
    ));
    let full_screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    reactor.handle_event(screen_params_event(vec![full_screen], vec![None], vec![]));

    reactor.handle_events(apps.make_app(1, make_windows(1)));

    let state_before = apps.windows.clone();
    let _events = apps.simulate_events();
    assert_eq!(state_before, apps.windows, "Window should not have been moved",);

    // Make sure it doesn't choke on destroyed events for ignored windows.
    reactor.handle_event(Event::WindowDestroyed(WindowId::new(1, 1)));
    reactor.handle_event(Event::WindowCreated(
        WindowId::new(1, 2),
        make_window(2),
        None,
        Some(MouseState::Up),
    ));
    reactor.handle_event(Event::WindowDestroyed(WindowId::new(1, 2)));
}

#[test]
fn it_keeps_discovered_windows_on_their_initial_screen() {
    let mut apps = Apps::new();
    let mut reactor = Reactor::new_for_test(LayoutEngine::new(
        &crate::common::config::VirtualWorkspaceSettings::default(),
        &crate::common::config::LayoutSettings::default(),
        None,
    ));
    let screen1 = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let screen2 = CGRect::new(CGPoint::new(1000., 0.), CGSize::new(1000., 1000.));
    reactor.handle_event(screen_params_event(
        vec![screen1, screen2],
        vec![Some(SpaceId::new(1)), Some(SpaceId::new(2))],
        vec![],
    ));

    let mut windows = make_windows(2);
    windows[1].frame.origin = CGPoint::new(1100., 100.);
    reactor.handle_events(apps.make_app(1, windows));

    let _events = apps.simulate_events();
    assert_eq!(
        screen1,
        apps.windows.get(&WindowId::new(1, 1)).expect("Window was not resized").frame,
    );
    assert_eq!(
        screen2,
        apps.windows.get(&WindowId::new(1, 2)).expect("Window was not resized").frame,
    );
}

#[test]
fn it_ignores_windows_on_nonzero_layers() {
    let mut apps = Apps::new();
    let mut reactor = Reactor::new_for_test(LayoutEngine::new(
        &crate::common::config::VirtualWorkspaceSettings::default(),
        &crate::common::config::LayoutSettings::default(),
        None,
    ));
    let full_screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    reactor.handle_event(screen_params_event(
        vec![full_screen],
        vec![Some(SpaceId::new(1))],
        vec![WindowServerInfo {
            id: WindowServerId::new(1),
            pid: 1,
            layer: 10,
            frame: CGRect::ZERO,
            min_frame: CGSize::ZERO,
            max_frame: CGSize::ZERO,
        }],
    ));

    reactor.handle_events(apps.make_app_with_opts(1, make_windows(1), None, true, false));

    let state_before = apps.windows.clone();
    let _events = apps.simulate_events();
    assert_eq!(state_before, apps.windows, "Window should not have been moved",);

    // Make sure it doesn't choke on destroyed events for ignored windows.
    reactor.handle_event(Event::WindowDestroyed(WindowId::new(1, 1)));
    reactor.handle_event(Event::WindowCreated(
        WindowId::new(1, 2),
        make_window(2),
        None,
        Some(MouseState::Up),
    ));
    reactor.handle_event(Event::WindowDestroyed(WindowId::new(1, 2)));
}

#[test]
fn handle_layout_response_groups_windows_by_app_and_screen() {
    let mut apps = Apps::new();
    let mut reactor = Reactor::new_for_test(LayoutEngine::new(
        &crate::common::config::VirtualWorkspaceSettings::default(),
        &crate::common::config::LayoutSettings::default(),
        None,
    ));
    let (raise_manager_tx, mut raise_manager_rx) = actor::channel();
    reactor.communication_manager.raise_manager_tx = raise_manager_tx;
    let screen1 = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let screen2 = CGRect::new(CGPoint::new(1000., 0.), CGSize::new(1000., 1000.));
    reactor.handle_event(screen_params_event(
        vec![screen1, screen2],
        vec![Some(SpaceId::new(1)), Some(SpaceId::new(2))],
        vec![],
    ));

    reactor.handle_events(apps.make_app(1, make_windows(2)));

    let mut windows = make_windows(2);
    windows[1].frame.origin = CGPoint::new(1100., 100.);
    reactor.handle_events(apps.make_app(2, windows));

    let _events = apps.simulate_events();
    while raise_manager_rx.try_recv().is_ok() {}

    reactor.handle_layout_response(
        layout::EventResponse {
            raise_windows: vec![
                WindowId::new(1, 1),
                WindowId::new(1, 2),
                WindowId::new(2, 1),
                WindowId::new(2, 2),
            ],
            focus_window: None,
            boundary_hit: None,
        },
        None,
    );
    let msg = raise_manager_rx.try_recv().expect("Should have sent an event").1;
    match msg {
        raise_manager::Event::RaiseRequest(RaiseRequest {
            raise_windows, focus_window, ..
        }) => {
            let raise_windows: HashSet<Vec<WindowId>> = raise_windows.into_iter().collect();
            let expected = [
                vec![WindowId::new(1, 1), WindowId::new(1, 2)],
                vec![WindowId::new(2, 1)],
                vec![WindowId::new(2, 2)],
            ]
            .into_iter()
            .collect();
            assert_eq!(raise_windows, expected);
            assert!(focus_window.is_none());
        }
        _ => panic!("Unexpected event: {msg:?}"),
    }
}

#[test]
fn handle_layout_response_includes_handles_for_raise_and_focus_windows() {
    let mut apps = Apps::new();
    let mut reactor = Reactor::new_for_test(LayoutEngine::new(
        &crate::common::config::VirtualWorkspaceSettings::default(),
        &crate::common::config::LayoutSettings::default(),
        None,
    ));
    let (raise_manager_tx, mut raise_manager_rx) = actor::channel();
    reactor.communication_manager.raise_manager_tx = raise_manager_tx;

    reactor.handle_events(apps.make_app(1, make_windows(1)));
    reactor.handle_events(apps.make_app(2, make_windows(1)));

    let _events = apps.simulate_events();
    while raise_manager_rx.try_recv().is_ok() {}
    reactor.handle_layout_response(
        layout::EventResponse {
            raise_windows: vec![WindowId::new(1, 1)],
            focus_window: Some(WindowId::new(2, 1)),
            boundary_hit: None,
        },
        None,
    );
    let msg = raise_manager_rx.try_recv().expect("Should have sent an event").1;
    match msg {
        raise_manager::Event::RaiseRequest(RaiseRequest { app_handles, .. }) => {
            assert!(app_handles.contains_key(&1));
            assert!(app_handles.contains_key(&2));
        }
        _ => panic!("Unexpected event: {msg:?}"),
    }
}

#[test]
fn focus_next_window_focuses_discovered_new_window() {
    let mut apps = Apps::new();
    let mut reactor = Reactor::new_for_test(LayoutEngine::new(
        &crate::common::config::VirtualWorkspaceSettings::default(),
        &crate::common::config::LayoutSettings::default(),
        None,
    ));
    let (raise_manager_tx, mut raise_manager_rx) = actor::channel();
    reactor.communication_manager.raise_manager_tx = raise_manager_tx;
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let space = SpaceId::new(1);
    reactor.handle_event(screen_params_event(vec![screen], vec![Some(space)], vec![]));

    reactor.handle_event(Event::Command(Command::Reactor(ReactorCommand::FocusNextWindow)));
    reactor.handle_events(apps.make_app(1, make_windows(1)));

    let mut focus_requests = Vec::new();
    while let Ok((_, msg)) = raise_manager_rx.try_recv() {
        if let raise_manager::Event::RaiseRequest(RaiseRequest {
            focus_window: Some((wid, _)),
            ..
        }) = msg
        {
            focus_requests.push(wid);
        }
    }

    assert!(
        focus_requests.contains(&WindowId::new(1, 1)),
        "FocusNextWindow should focus the first manageable window discovered after an exec"
    );
    assert_eq!(
        reactor.layout_manager.layout_engine.selected_window(space),
        Some(WindowId::new(1, 1))
    );
}

#[test]
fn focus_next_window_assigns_exec_window_to_command_workspace() {
    let mut vw_settings = crate::common::config::VirtualWorkspaceSettings::default();
    vw_settings.display_default_workspaces.insert("test-display-0".into(), 1);
    vw_settings.display_default_workspaces.insert("test-display-1".into(), 2);

    let mut reactor = Reactor::new_for_test(LayoutEngine::new(
        &vw_settings,
        &crate::common::config::LayoutSettings::default(),
        None,
    ));
    let screen1 = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let screen2 = CGRect::new(CGPoint::new(1000., 0.), CGSize::new(1000., 1000.));
    let space1 = SpaceId::new(1);
    let space2 = SpaceId::new(2);
    reactor.handle_event(screen_params_event(
        vec![screen1, screen2],
        vec![Some(space1), Some(space2)],
        vec![],
    ));

    let _ = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager_mut()
        .list_workspaces(space1);
    let _ = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager_mut()
        .list_workspaces(space2);

    let ws1 = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager()
        .active_workspace(space1)
        .expect("space1 should initialize directly to ws1");

    let mut apps = Apps::new();
    reactor.handle_events(apps.make_app_with_opts(
        10,
        make_windows(1),
        Some(WindowId::new(10, 1)),
        true,
        true,
    ));
    reactor.handle_event(Event::ApplicationGloballyActivated(10));
    assert_eq!(reactor.main_window_space(), Some(space1));

    reactor.request_focus_next_window();
    assert_eq!(
        reactor.refocus_manager.focus_next_window_target.map(|target| target.space),
        Some(space1),
        "precondition: exec target should be captured from the command workspace",
    );
    assert_eq!(
        reactor.refocus_manager.focus_next_window_target.map(|target| target.workspace_id),
        Some(ws1),
        "precondition: exec target should capture ws1",
    );

    let mut launched = make_window(1);
    launched.sys_id = Some(WindowServerId::new(501));
    launched.frame.origin = CGPoint::new(screen2.origin.x + 100.0, screen2.origin.y + 100.0);
    reactor.handle_events(apps.make_app_with_opts(
        50,
        vec![launched],
        Some(WindowId::new(50, 1)),
        true,
        true,
    ));

    let launched_window = WindowId::new(50, 1);
    let vwm = reactor.layout_manager.layout_engine.virtual_workspace_manager();
    assert_eq!(
        vwm.workspace_for_window(launched_window),
        Some(ws1),
        "exec-launched window should be assigned to the command workspace, not the display where macOS first placed it",
    );
    assert_eq!(vwm.workspace_space(ws1), Some(space1));
    assert_eq!(
        reactor.layout_manager.layout_engine.selected_window(space1),
        Some(launched_window),
        "exec-launched window should be focused on the command workspace",
    );
}

#[test]
fn canceled_focus_next_window_does_not_focus_discovered_window() {
    let mut apps = Apps::new();
    let mut reactor = Reactor::new_for_test(LayoutEngine::new(
        &crate::common::config::VirtualWorkspaceSettings::default(),
        &crate::common::config::LayoutSettings::default(),
        None,
    ));
    let (raise_manager_tx, mut raise_manager_rx) = actor::channel();
    reactor.communication_manager.raise_manager_tx = raise_manager_tx;
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    reactor.handle_event(screen_params_event(
        vec![screen],
        vec![Some(SpaceId::new(1))],
        vec![],
    ));

    reactor.handle_event(Event::Command(Command::Reactor(ReactorCommand::FocusNextWindow)));
    reactor.handle_event(Event::Command(Command::Reactor(ReactorCommand::CancelFocusNextWindow)));
    reactor.handle_events(apps.make_app(1, make_windows(1)));

    while let Ok((_, msg)) = raise_manager_rx.try_recv() {
        if let raise_manager::Event::RaiseRequest(RaiseRequest {
            focus_window: Some((WindowId { pid: 1, .. }, _)),
            ..
        }) = msg
        {
            panic!("CancelFocusNextWindow should cancel the pending exec focus request");
        }
    }
}

#[test]
fn workspace_switch_batches_all_windows_with_eui_enabled() {
    let mut apps = Apps::new();
    let mut reactor = Reactor::new_for_test(LayoutEngine::new(
        &crate::common::config::VirtualWorkspaceSettings::default(),
        &crate::common::config::LayoutSettings::default(),
        None,
    ));
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let space = SpaceId::new(1);

    reactor.handle_event(screen_params_event(vec![screen], vec![Some(space)], vec![]));
    reactor.handle_events(apps.make_app(1, make_windows(2)));
    apps.simulate_until_quiet(&mut reactor);
    let _ = apps.requests();

    // Phase 3 lazy init only seeds slot 0 per display. The
    // `MoveWindowToWorkspace { workspace: 1 }` / `SwitchToWorkspace(1)`
    // commands resolve `workspace` as a per-space *index* into
    // `list_workspaces`, so we have to materialise a second workspace on this
    // space before the index reaches it. Phase 4 will replace these
    // index-based commands with global slot numbers; until then the test
    // pre-allocates the slot it intends to exercise. We also re-fire
    // SpaceExposed so the engine wires up `workspace_layouts` for the new
    // workspace; otherwise MoveWindowToWorkspace would silently skip the
    // tree-insert because no layout exists for the target.
    let space_uuid = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager()
        .space_display(space)
        .expect("space has a display uuid after screen-params handling")
        .to_owned();
    let _ = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager_mut()
        .create_workspace_with_number(1, &space_uuid, space);
    let _ = reactor
        .layout_manager
        .layout_engine
        .handle_event(LayoutEvent::SpaceExposed(space, screen.size));

    reactor.handle_event(Event::Command(Command::Layout(
        LayoutCommand::MoveWindowToWorkspace {
            workspace: 1,
            window_id: Some(2),
        },
    )));
    apps.simulate_until_quiet(&mut reactor);
    let _ = apps.requests();

    reactor.handle_event(Event::Command(Command::Layout(
        LayoutCommand::SwitchToWorkspace(1),
    )));

    let requests = apps.requests();
    assert!(
        requests.iter().any(|req| {
            matches!(
                req,
                Request::SetBatchWindowFrame(frames, _, true)
                    if frames.iter().any(|(wid, _)| *wid == WindowId::new(1, 1))
                        && frames.iter().any(|(wid, _)| *wid == WindowId::new(1, 2))
            )
        }),
        "expected workspace-switch batch to disable eui for both hidden and visible windows: {requests:?}"
    );
}

#[test]
fn auto_workspace_switch_focuses_activated_window_not_stale_workspace_focus() {
    let mut apps = Apps::new();
    let mut reactor = Reactor::new_for_test(LayoutEngine::new(
        &crate::common::config::VirtualWorkspaceSettings::default(),
        &crate::common::config::LayoutSettings::default(),
        None,
    ));
    let (raise_manager_tx, mut raise_manager_rx) = actor::channel();
    reactor.communication_manager.raise_manager_tx = raise_manager_tx;

    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let space = SpaceId::new(1);
    let stale_focus = WindowId::new(1, 1);
    let activated = WindowId::new(2, 1);

    reactor.handle_event(screen_params_event(vec![screen], vec![Some(space)], vec![]));
    reactor.handle_events(apps.make_app(1, make_windows(1)));
    reactor.handle_events(apps.make_app(2, make_windows(1)));
    apps.simulate_until_quiet(&mut reactor);

    reactor.send_layout_event(LayoutEvent::WindowFocused(space, stale_focus));
    reactor.handle_event(Event::Command(Command::Layout(
        LayoutCommand::MoveWindowToWorkspace { workspace: 1, window_id: None },
    )));
    apps.simulate_until_quiet(&mut reactor);

    reactor.send_layout_event(LayoutEvent::WindowFocused(space, activated));
    reactor.handle_event(Event::Command(Command::Layout(
        LayoutCommand::MoveWindowToWorkspace { workspace: 1, window_id: None },
    )));
    apps.simulate_until_quiet(&mut reactor);

    reactor.handle_event(Event::Command(Command::Layout(
        LayoutCommand::SwitchToWorkspace(1),
    )));
    reactor.send_layout_event(LayoutEvent::WindowFocused(space, stale_focus));
    reactor.handle_event(Event::Command(Command::Layout(
        LayoutCommand::SwitchToWorkspace(0),
    )));
    while raise_manager_rx.try_recv().is_ok() {}

    reactor.maybe_auto_switch_to_window_workspace(activated.pid, activated, space);

    let msg = raise_manager_rx.try_recv().expect("Should have sent an event").1;
    match msg {
        raise_manager::Event::RaiseRequest(RaiseRequest { focus_window, focus_quiet, .. }) => {
            assert_eq!(focus_window.map(|(wid, _)| wid), Some(activated));
            assert_eq!(focus_quiet, Quiet::Yes);
        }
        _ => panic!("Unexpected event: {msg:?}"),
    }
}

#[test]
fn windows_discovered_does_not_reintroduce_inactive_workspace_window() {
    let mut apps = Apps::new();
    let mut reactor = Reactor::new_for_test(LayoutEngine::new(
        &crate::common::config::VirtualWorkspaceSettings::default(),
        &crate::common::config::LayoutSettings::default(),
        None,
    ));
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let space = SpaceId::new(1);

    reactor.handle_event(screen_params_event(vec![screen], vec![Some(space)], vec![]));
    reactor.handle_events(apps.make_app(1, make_windows(2)));
    apps.simulate_until_quiet(&mut reactor);

    // See `workspace_switch_batches_all_windows_with_eui_enabled` for why we
    // pre-create slot 1 and re-fire SpaceExposed: in Phase 3 lazy init only
    // seeds slot 0 per display, the index-based MoveWindowToWorkspace command
    // would otherwise resolve to nothing, and the engine needs SpaceExposed
    // to wire up the new workspace's layout tree.
    let space_uuid = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager()
        .space_display(space)
        .expect("space has a display uuid after screen-params handling")
        .to_owned();
    let _ = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager_mut()
        .create_workspace_with_number(1, &space_uuid, space);
    let _ = reactor
        .layout_manager
        .layout_engine
        .handle_event(LayoutEvent::SpaceExposed(space, screen.size));

    reactor.handle_event(Event::Command(Command::Layout(
        LayoutCommand::MoveWindowToWorkspace {
            workspace: 1,
            window_id: Some(2),
        },
    )));
    apps.simulate_until_quiet(&mut reactor);

    reactor.handle_event(Event::Command(Command::Layout(
        LayoutCommand::SwitchToWorkspace(1),
    )));
    apps.simulate_until_quiet(&mut reactor);

    reactor.handle_event(Event::WindowsDiscovered {
        pid: 1,
        new: vec![],
        known_visible: vec![WindowId::new(1, 1), WindowId::new(1, 2)],
    });

    assert_eq!(
        reactor.layout_manager.layout_engine.windows_in_active_workspace(space),
        vec![WindowId::new(1, 2)],
    );
}

#[test]
fn it_preserves_layout_after_login_screen() {
    // TODO: This would be better tested with a more complete simulation.
    let mut apps = Apps::new();
    let mut reactor = Reactor::new_for_test(LayoutEngine::new(
        &crate::common::config::VirtualWorkspaceSettings::default(),
        &crate::common::config::LayoutSettings::default(),
        None,
    ));
    let space = SpaceId::new(1);
    let full_screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    reactor.handle_event(screen_params_event(vec![full_screen], vec![Some(space)], vec![]));

    reactor.handle_events(apps.make_app_with_opts(
        1,
        make_windows(3),
        Some(WindowId::new(1, 1)),
        true,
        true,
    ));
    reactor.handle_event(Event::ApplicationGloballyActivated(1));
    apps.simulate_until_quiet(&mut reactor);
    let default = reactor.layout_manager.layout_engine.calculate_layout(
        space,
        full_screen,
        &reactor.config.settings.layout.gaps,
        0.0,
        crate::common::config::HorizontalPlacement::Top,
        crate::common::config::VerticalPlacement::Right,
    );

    assert!(reactor.layout_manager.layout_engine.selected_window(space).is_some());
    reactor.handle_event(Event::Command(Command::Layout(LayoutCommand::MoveNode(
        Direction::Up,
    ))));
    apps.simulate_until_quiet(&mut reactor);
    let modified = reactor.layout_manager.layout_engine.calculate_layout(
        space,
        full_screen,
        &reactor.config.settings.layout.gaps,
        0.0,
        crate::common::config::HorizontalPlacement::Top,
        crate::common::config::VerticalPlacement::Right,
    );
    assert_ne!(default, modified);

    reactor.handle_event(screen_params_event(vec![CGRect::ZERO], vec![None], vec![]));
    reactor.handle_event(screen_params_event(
        vec![full_screen],
        vec![Some(space)],
        (1..=3)
            .map(|n| WindowServerInfo {
                pid: 1,
                id: WindowServerId::new(n),
                layer: 0,
                frame: CGRect::ZERO,
                min_frame: CGSize::ZERO,
                max_frame: CGSize::ZERO,
            })
            .collect(),
    ));
    let requests = apps.requests();
    for request in requests {
        match request {
            Request::GetVisibleWindows => {
                // Simulate the login screen condition: No windows are
                // considered visible by the accessibility API, but they are
                // from the window server API in the event above.
                reactor.handle_event(Event::WindowsDiscovered {
                    pid: 1,
                    new: vec![],
                    known_visible: vec![],
                });
            }
            req => {
                let events = apps.simulate_events_for_requests(vec![req]);
                for event in events {
                    reactor.handle_event(event);
                }
            }
        }
    }
    apps.simulate_until_quiet(&mut reactor);

    assert_eq!(
        reactor.layout_manager.layout_engine.calculate_layout(
            space,
            full_screen,
            &reactor.config.settings.layout.gaps,
            0.0,
            crate::common::config::HorizontalPlacement::Top,
            crate::common::config::VerticalPlacement::Right,
        ),
        modified
    );
}

#[test]
fn title_change_reapply_does_not_rebalance_unchanged_layout() {
    let mut apps = Apps::new();
    let mut reactor = Reactor::new_for_test(LayoutEngine::new(
        &crate::common::config::VirtualWorkspaceSettings::default(),
        &crate::common::config::LayoutSettings::default(),
        None,
    ));
    reactor.config.virtual_workspaces.reapply_app_rules_on_title_change = true;

    let space = SpaceId::new(1);
    let full_screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    reactor.handle_event(screen_params_event(vec![full_screen], vec![Some(space)], vec![]));

    reactor.handle_events(apps.make_app_with_opts(
        1,
        make_windows(3),
        Some(WindowId::new(1, 1)),
        true,
        true,
    ));
    reactor.handle_event(Event::ApplicationGloballyActivated(1));
    apps.simulate_until_quiet(&mut reactor);

    assert!(reactor.layout_manager.layout_engine.selected_window(space).is_some());
    reactor.handle_event(Event::Command(Command::Layout(LayoutCommand::MoveNode(
        Direction::Up,
    ))));
    apps.simulate_until_quiet(&mut reactor);

    let modified = reactor.layout_manager.layout_engine.calculate_layout(
        space,
        full_screen,
        &reactor.config.settings.layout.gaps,
        0.0,
        crate::common::config::HorizontalPlacement::Top,
        crate::common::config::VerticalPlacement::Right,
    );

    reactor.handle_event(Event::WindowTitleChanged(
        WindowId::new(1, 1),
        "Renamed window".to_string(),
    ));

    assert_eq!(
        reactor.layout_manager.layout_engine.calculate_layout(
            space,
            full_screen,
            &reactor.config.settings.layout.gaps,
            0.0,
            crate::common::config::HorizontalPlacement::Top,
            crate::common::config::VerticalPlacement::Right,
        ),
        modified
    );
}

#[test]
fn title_change_reapply_does_not_rebalance_when_window_stays_floating() {
    let mut apps = Apps::new();
    let mut reactor = Reactor::new_for_test(LayoutEngine::new(
        &crate::common::config::VirtualWorkspaceSettings::default(),
        &crate::common::config::LayoutSettings::default(),
        None,
    ));
    reactor.config.virtual_workspaces.reapply_app_rules_on_title_change = true;

    let space = SpaceId::new(1);
    let full_screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    reactor.handle_event(screen_params_event(vec![full_screen], vec![Some(space)], vec![]));

    reactor.handle_events(apps.make_app_with_opts(
        1,
        make_windows(3),
        Some(WindowId::new(1, 1)),
        true,
        true,
    ));
    reactor.handle_event(Event::ApplicationGloballyActivated(1));
    apps.simulate_until_quiet(&mut reactor);

    assert!(reactor.layout_manager.layout_engine.selected_window(space).is_some());
    reactor.handle_event(Event::Command(Command::Layout(LayoutCommand::MoveNode(
        Direction::Up,
    ))));
    apps.simulate_until_quiet(&mut reactor);

    reactor.handle_event(Event::Command(Command::Layout(
        LayoutCommand::ToggleWindowFloating,
    )));
    apps.simulate_until_quiet(&mut reactor);
    assert!(reactor.layout_manager.layout_engine.is_window_floating(WindowId::new(1, 1)));

    let modified = reactor.layout_manager.layout_engine.calculate_layout(
        space,
        full_screen,
        &reactor.config.settings.layout.gaps,
        0.0,
        crate::common::config::HorizontalPlacement::Top,
        crate::common::config::VerticalPlacement::Right,
    );

    reactor.handle_event(Event::WindowTitleChanged(
        WindowId::new(1, 1),
        "Renamed floating window".to_string(),
    ));

    assert!(reactor.layout_manager.layout_engine.is_window_floating(WindowId::new(1, 1)));
    assert_eq!(
        reactor.layout_manager.layout_engine.calculate_layout(
            space,
            full_screen,
            &reactor.config.settings.layout.gaps,
            0.0,
            crate::common::config::HorizontalPlacement::Top,
            crate::common::config::VerticalPlacement::Right,
        ),
        modified
    );
}

#[test]
fn menu_open_state_is_cleared_when_owner_deactivates() {
    let mut reactor = Reactor::new_for_test(LayoutEngine::new(
        &crate::common::config::VirtualWorkspaceSettings::default(),
        &crate::common::config::LayoutSettings::default(),
        None,
    ));
    let (event_tap_tx, mut event_tap_rx) = actor::channel();
    reactor.communication_manager.event_tap_tx = Some(event_tap_tx);

    reactor.handle_event(Event::MenuOpened(1));
    let disable = event_tap_rx.try_recv().expect("menu-open should update event tap").1;
    assert!(matches!(
        disable,
        crate::actor::event_tap::Request::SetFocusFollowsMouseEnabled(false)
    ));
    assert_eq!(reactor.menu_manager.menu_state, MenuState::Open(1));

    reactor.handle_event(Event::ApplicationDeactivated(1));
    let enable = event_tap_rx
        .try_recv()
        .expect("app deactivation should re-enable focus-follows-mouse")
        .1;
    assert!(matches!(
        enable,
        crate::actor::event_tap::Request::SetFocusFollowsMouseEnabled(true)
    ));
    assert_eq!(reactor.menu_manager.menu_state, MenuState::Closed);
}

#[test]
fn stale_menu_open_state_is_cleared_when_other_app_activates() {
    let mut reactor = Reactor::new_for_test(LayoutEngine::new(
        &crate::common::config::VirtualWorkspaceSettings::default(),
        &crate::common::config::LayoutSettings::default(),
        None,
    ));
    let (event_tap_tx, mut event_tap_rx) = actor::channel();
    reactor.communication_manager.event_tap_tx = Some(event_tap_tx);

    reactor.handle_event(Event::MenuOpened(1));
    let _ = event_tap_rx.try_recv().expect("menu-open should update event tap");
    assert_eq!(reactor.menu_manager.menu_state, MenuState::Open(1));

    reactor.handle_event(Event::ApplicationGloballyActivated(2));
    let enable = event_tap_rx
        .try_recv()
        .expect("activation of another app should clear stale menu state")
        .1;
    assert!(matches!(
        enable,
        crate::actor::event_tap::Request::SetFocusFollowsMouseEnabled(true)
    ));
    assert_eq!(reactor.menu_manager.menu_state, MenuState::Closed);
}

#[test]
fn it_retains_windows_without_server_ids_after_login_visibility_failure() {
    let mut apps = Apps::new();
    let mut reactor = Reactor::new_for_test(LayoutEngine::new(
        &crate::common::config::VirtualWorkspaceSettings::default(),
        &crate::common::config::LayoutSettings::default(),
        None,
    ));
    let space = SpaceId::new(1);
    let full_screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    reactor.handle_event(screen_params_event(vec![full_screen], vec![Some(space)], vec![]));

    let window = WindowInfo {
        is_standard: true,
        is_root: true,
        is_minimized: false,
        is_resizable: true,
        min_size: None,
        max_size: None,
        title: "NoServerId".to_string(),
        frame: CGRect::new(CGPoint::new(50., 50.), CGSize::new(400., 400.)),
        sys_id: None,
        bundle_id: None,
        path: None,
        ax_role: None,
        ax_subrole: None,
    };

    reactor.handle_events(apps.make_app_with_opts(
        1,
        vec![window],
        Some(WindowId::new(1, 1)),
        true,
        false,
    ));
    apps.simulate_until_quiet(&mut reactor);

    reactor.handle_event(Event::SpaceChanged(vec![None]));

    // Simulate a native fullscreen transition: space temporarily becomes a fullscreen
    // space id (reactor suppresses it to None), then returns to the original space.
    let fullscreen_space = SpaceId::new(0x400000000 + space.get());
    reactor.handle_event(Event::SpaceChanged(vec![Some(fullscreen_space)]));

    reactor.handle_event(Event::SpaceChanged(vec![Some(space)]));

    loop {
        let requests = apps.requests();
        if requests.is_empty() {
            break;
        }

        let mut other_requests = Vec::new();
        for request in requests {
            match request {
                Request::GetVisibleWindows => {
                    reactor.handle_event(Event::WindowsDiscovered {
                        pid: 1,
                        new: vec![],
                        known_visible: vec![],
                    });
                }
                other => other_requests.push(other),
            }
        }

        if !other_requests.is_empty() {
            let events = apps.simulate_events_for_requests(other_requests);
            for event in events {
                reactor.handle_event(event);
            }
        }
    }
}

#[test]
fn animated_layout_handles_windows_without_server_ids() {
    let mut apps = Apps::new();
    let mut reactor = Reactor::new_for_test(LayoutEngine::new(
        &crate::common::config::VirtualWorkspaceSettings::default(),
        &crate::common::config::LayoutSettings::default(),
        None,
    ));
    let space = SpaceId::new(1);
    reactor.handle_event(screen_params_event(
        vec![CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.))],
        vec![Some(space)],
        vec![],
    ));

    let mut window = make_window(1);
    window.sys_id = None;
    window.frame = CGRect::new(CGPoint::new(50., 50.), CGSize::new(400., 400.));

    reactor.handle_events(apps.make_app_with_opts(
        1,
        vec![window],
        Some(WindowId::new(1, 1)),
        true,
        false,
    ));
    apps.requests();

    let target = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    assert!(super::animation::AnimationManager::animate_layout(
        &mut reactor,
        space,
        &[(WindowId::new(1, 1), target)],
        true,
        None,
    ));

    let requests = apps.requests();
    assert!(
        requests.iter().any(|request| matches!(
            request,
            Request::SetWindowFrame(..) | Request::SetBatchWindowFrame(..)
        )),
        "expected layout to still request a frame update without a server id: {requests:?}"
    );
}

#[test]
fn display_index_selector_uses_physical_left_to_right_order() {
    let mut reactor = Reactor::new_for_test(LayoutEngine::new(
        &crate::common::config::VirtualWorkspaceSettings::default(),
        &crate::common::config::LayoutSettings::default(),
        None,
    ));
    let right = CGRect::new(CGPoint::new(200000., 0.), CGSize::new(1000., 1000.));
    let left = CGRect::new(CGPoint::new(100000., 0.), CGSize::new(1000., 1000.));
    reactor.handle_event(screen_params_event(
        vec![right, left],
        vec![Some(SpaceId::new(1)), Some(SpaceId::new(2))],
        vec![],
    ));

    let selected = reactor
        .screen_for_selector(&DisplaySelector::Index(0), None)
        .expect("expected display index 0 to resolve");

    assert_eq!(selected.frame, left);
}

#[test]
fn display_churn_quarantine_counters_increment() {
    let mut reactor = Reactor::new_for_test(LayoutEngine::new(
        &crate::common::config::VirtualWorkspaceSettings::default(),
        &crate::common::config::LayoutSettings::default(),
        None,
    ));
    reactor.display_topology_manager.quarantine_appeared();
    reactor.display_topology_manager.quarantine_destroyed();
    reactor.display_topology_manager.quarantine_resync();

    let stats = reactor.display_topology_manager.quarantine_stats.clone();
    assert_eq!(stats.appeared_dropped, 1);
    assert_eq!(stats.destroyed_dropped, 1);
    assert_eq!(stats.resync_dropped, 1);
}

#[test]
fn display_churn_transitions_to_awaiting_commit_then_stable() {
    let mut reactor = Reactor::new_for_test(LayoutEngine::new(
        &crate::common::config::VirtualWorkspaceSettings::default(),
        &crate::common::config::LayoutSettings::default(),
        None,
    ));
    let frame = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let space = SpaceId::new(1);
    reactor.handle_event(screen_params_event(vec![frame], vec![Some(space)], vec![]));

    reactor.display_topology_manager.begin_churn(
        2,
        crate::sys::skylight::DisplayReconfigFlags::ADD,
        crate::common::collections::HashSet::default(),
    );
    reactor
        .display_topology_manager
        .end_churn_to_awaiting(2, crate::sys::skylight::DisplayReconfigFlags::ADD);

    assert!(matches!(
        reactor.display_topology_manager.state(),
        TopologyState::AwaitingCommitSnapshot { .. }
    ));

    reactor.handle_event(screen_params_event(vec![frame], vec![Some(space)], vec![]));

    assert!(matches!(
        reactor.display_topology_manager.state(),
        TopologyState::Stable
    ));
}

#[test]
fn display_churn_quarantines_window_frame_changed_events() {
    let mut reactor = Reactor::new_for_test(LayoutEngine::new(
        &crate::common::config::VirtualWorkspaceSettings::default(),
        &crate::common::config::LayoutSettings::default(),
        None,
    ));
    reactor.display_topology_manager.begin_churn(
        3,
        crate::sys::skylight::DisplayReconfigFlags::ADD,
        crate::common::collections::HashSet::default(),
    );

    let quarantined = reactor.maybe_quarantine_during_churn(&Event::WindowFrameChanged(
        WindowId::new(99, 1),
        CGRect::new(CGPoint::new(10., 10.), CGSize::new(500., 400.)),
        None,
        Requested(false),
        Some(MouseState::Up),
    ));
    assert!(
        quarantined,
        "WindowFrameChanged should be quarantined during churn"
    );
}

#[test]
fn normal_macos_space_switch_does_not_arm_topology_relayout() {
    let mut reactor = Reactor::new_for_test(LayoutEngine::new(
        &crate::common::config::VirtualWorkspaceSettings::default(),
        &crate::common::config::LayoutSettings::default(),
        None,
    ));

    let left = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1280., 800.));
    let right = CGRect::new(CGPoint::new(1280., 0.), CGSize::new(1280., 800.));

    reactor.handle_event(screen_params_event(
        vec![left, right],
        vec![Some(SpaceId::new(11)), Some(SpaceId::new(22))],
        vec![],
    ));
    assert!(!reactor.pending_space_change_manager.topology_relayout_pending);

    reactor.handle_event(screen_params_event(
        vec![left, right],
        vec![Some(SpaceId::new(111)), Some(SpaceId::new(222))],
        vec![],
    ));
    assert!(
        !reactor.pending_space_change_manager.topology_relayout_pending,
        "Normal same-display macOS Space switches must not be treated as display topology changes"
    );
    assert_eq!(
        reactor.raw_spaces_for_current_screens(),
        vec![Some(SpaceId::new(111)), Some(SpaceId::new(222))],
        "Screen state should still advance to the newly active macOS spaces"
    );
    assert!(reactor.is_space_active(SpaceId::new(111)));
    assert!(reactor.is_space_active(SpaceId::new(222)));
}

#[test]
fn fullscreen_space_in_screen_params_does_not_trigger_topology_relayout() {
    let mut reactor = Reactor::new_for_test(LayoutEngine::new(
        &crate::common::config::VirtualWorkspaceSettings::default(),
        &crate::common::config::LayoutSettings::default(),
        None,
    ));

    let frame = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1280., 800.));
    let user_space = SpaceId::new(11);
    let fullscreen_space = SpaceId::new(0x400000000 + user_space.get());
    let display_uuid = "11111111-1111-1111-1111-111111111111".to_string();
    let screens_for = |space: SpaceId| -> Vec<ScreenInfo> {
        vec![ScreenInfo {
            id: crate::sys::screen::ScreenId::new(0),
            frame,
            space: Some(space),
            display_uuid: display_uuid.clone(),
            name: None,
        }]
    };

    reactor.handle_event(Event::ScreenParametersChanged(screens_for(user_space)));
    assert!(!reactor.pending_space_change_manager.topology_relayout_pending);
    assert_eq!(
        reactor.layout_manager.layout_engine.last_space_for_display_uuid(&display_uuid),
        Some(user_space)
    );

    reactor
        .space_manager
        .fullscreen_by_space
        .insert(fullscreen_space.get(), FullscreenSpaceTrack::default());
    reactor.handle_event(Event::ScreenParametersChanged(screens_for(fullscreen_space)));
    assert!(
        !reactor.pending_space_change_manager.topology_relayout_pending,
        "fullscreen space transitions should not arm topology relayout"
    );
    assert_eq!(
        reactor.layout_manager.layout_engine.last_space_for_display_uuid(&display_uuid),
        Some(user_space),
        "fullscreen spaces should not replace display->user-space history"
    );

    reactor.handle_event(Event::ScreenParametersChanged(screens_for(user_space)));
    assert!(!reactor.pending_space_change_manager.topology_relayout_pending);
    assert_eq!(
        reactor.layout_manager.layout_engine.last_space_for_display_uuid(&display_uuid),
        Some(user_space)
    );
}

#[test]
fn fullscreen_screen_params_preserves_other_display_space() {
    let mut reactor = Reactor::new_for_test(LayoutEngine::new(
        &crate::common::config::VirtualWorkspaceSettings::default(),
        &crate::common::config::LayoutSettings::default(),
        None,
    ));

    let left = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let right = CGRect::new(CGPoint::new(1000., 0.), CGSize::new(1000., 1000.));
    let left_space_2 = SpaceId::new(12);
    let left_space_1 = SpaceId::new(11);
    let right_space_1 = SpaceId::new(21);
    let right_fullscreen = SpaceId::new(0x400000000 + right_space_1.get());

    reactor.handle_event(screen_params_event(
        vec![left, right],
        vec![Some(left_space_2), Some(right_space_1)],
        vec![],
    ));
    reactor
        .space_manager
        .fullscreen_by_space
        .insert(right_fullscreen.get(), FullscreenSpaceTrack::default());

    reactor.handle_event(screen_params_event(
        vec![left, right],
        vec![Some(left_space_1), Some(right_fullscreen)],
        vec![],
    ));

    assert_eq!(
        reactor.raw_spaces_for_current_screens(),
        vec![Some(left_space_2), None],
        "Entering fullscreen on one display must not accept a transient user-space change on another display"
    );
}

#[test]
fn fullscreen_space_changed_preserves_other_display_space() {
    let mut reactor = Reactor::new_for_test(LayoutEngine::new(
        &crate::common::config::VirtualWorkspaceSettings::default(),
        &crate::common::config::LayoutSettings::default(),
        None,
    ));

    let left = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let right = CGRect::new(CGPoint::new(1000., 0.), CGSize::new(1000., 1000.));
    let left_space_2 = SpaceId::new(12);
    let left_space_1 = SpaceId::new(11);
    let right_space_1 = SpaceId::new(21);
    let right_fullscreen = SpaceId::new(0x400000000 + right_space_1.get());

    reactor.handle_event(screen_params_event(
        vec![left, right],
        vec![Some(left_space_2), Some(right_space_1)],
        vec![],
    ));
    reactor
        .space_manager
        .fullscreen_by_space
        .insert(right_fullscreen.get(), FullscreenSpaceTrack::default());

    reactor.handle_event(Event::SpaceChanged(vec![
        Some(left_space_1),
        Some(right_fullscreen),
    ]));

    assert_eq!(
        reactor.raw_spaces_for_current_screens(),
        vec![Some(left_space_2), None],
        "Fullscreen SpaceChanged snapshots must preserve unrelated displays' previous user spaces"
    );
}

#[test]
fn user_space_switch_is_allowed_while_other_display_already_fullscreen() {
    let mut reactor = Reactor::new_for_test(LayoutEngine::new(
        &crate::common::config::VirtualWorkspaceSettings::default(),
        &crate::common::config::LayoutSettings::default(),
        None,
    ));

    let left = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let right = CGRect::new(CGPoint::new(1000., 0.), CGSize::new(1000., 1000.));
    let left_space_2 = SpaceId::new(12);
    let left_space_1 = SpaceId::new(11);
    let right_space_1 = SpaceId::new(21);
    let right_fullscreen = SpaceId::new(0x400000000 + right_space_1.get());

    reactor.handle_event(screen_params_event(
        vec![left, right],
        vec![Some(left_space_2), Some(right_space_1)],
        vec![],
    ));
    reactor
        .space_manager
        .fullscreen_by_space
        .insert(right_fullscreen.get(), FullscreenSpaceTrack::default());
    reactor.handle_event(Event::SpaceChanged(vec![
        Some(left_space_2),
        Some(right_fullscreen),
    ]));

    reactor.handle_event(Event::SpaceChanged(vec![
        Some(left_space_1),
        Some(right_fullscreen),
    ]));

    assert_eq!(
        reactor.raw_spaces_for_current_screens(),
        vec![Some(left_space_1), None],
        "Once another display is already fullscreen, user space switches on this display should still be accepted"
    );
}

#[test]
fn fullscreen_screen_params_preserves_window_layout() {
    // Regression test for #308: waking from sleep while a fullscreen video is
    // active should not wipe workspace assignments.
    let mut apps = Apps::new();
    let mut reactor = Reactor::new_for_test(LayoutEngine::new(
        &crate::common::config::VirtualWorkspaceSettings::default(),
        &crate::common::config::LayoutSettings::default(),
        None,
    ));

    let user_space = SpaceId::new(1);
    let fullscreen_space = SpaceId::new(0x400000000 + user_space.get());
    let full_screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));

    // Set up a display with a user space and some windows.
    reactor.handle_event(screen_params_event(
        vec![full_screen],
        vec![Some(user_space)],
        vec![],
    ));
    reactor.handle_events(apps.make_app_with_opts(
        1,
        make_windows(3),
        Some(WindowId::new(1, 1)),
        true,
        true,
    ));
    reactor.handle_event(Event::ApplicationGloballyActivated(1));
    apps.simulate_until_quiet(&mut reactor);

    // Rearrange layout so we can detect if it gets reset.
    reactor.handle_event(Event::Command(Command::Layout(LayoutCommand::MoveNode(
        Direction::Up,
    ))));
    apps.simulate_until_quiet(&mut reactor);
    let layout_before = reactor.layout_manager.layout_engine.calculate_layout(
        user_space,
        full_screen,
        &reactor.config.settings.layout.gaps,
        0.0,
        crate::common::config::HorizontalPlacement::Top,
        crate::common::config::VerticalPlacement::Right,
    );

    // Simulate sleep/wake while fullscreen: ScreenParametersChanged arrives
    // with the fullscreen space id.
    reactor
        .space_manager
        .fullscreen_by_space
        .insert(fullscreen_space.get(), FullscreenSpaceTrack::default());
    reactor.handle_event(Event::ScreenParametersChanged(vec![ScreenInfo {
        id: crate::sys::screen::ScreenId::new(0),
        frame: full_screen,
        space: Some(fullscreen_space),
        display_uuid: "test-display-0".to_string(),
        name: None,
    }]));
    apps.simulate_until_quiet(&mut reactor);

    // The fullscreen space must not become the active space for the screen.
    assert_eq!(
        reactor.space_manager.screens[0].space, None,
        "fullscreen space should be nulled out, not stored as screen space"
    );

    // Return to user space (simulates exiting fullscreen).
    reactor.handle_event(screen_params_event(
        vec![full_screen],
        vec![Some(user_space)],
        vec![],
    ));
    apps.simulate_until_quiet(&mut reactor);

    let layout_after = reactor.layout_manager.layout_engine.calculate_layout(
        user_space,
        full_screen,
        &reactor.config.settings.layout.gaps,
        0.0,
        crate::common::config::HorizontalPlacement::Top,
        crate::common::config::VerticalPlacement::Right,
    );
    assert_eq!(
        layout_before, layout_after,
        "Window layout on user space must be preserved across fullscreen ScreenParametersChanged"
    );
}

// Helper: check whether any window owned by `pid` appears in the layout tree for `space`.
fn has_windows_in_layout(
    reactor: &mut Reactor,
    space: SpaceId,
    screen: CGRect,
    pid: pid_t,
) -> bool {
    let gaps = reactor.config.settings.layout.gaps.clone();
    reactor
        .layout_manager
        .layout_engine
        .calculate_layout(space, screen, &gaps, 0.0, Default::default(), Default::default())
        .iter()
        .any(|(wid, _)| wid.pid == pid)
}

fn has_window_in_layout(
    reactor: &mut Reactor,
    space: SpaceId,
    screen: CGRect,
    wid: WindowId,
) -> bool {
    let gaps = reactor.config.settings.layout.gaps.clone();
    reactor
        .layout_manager
        .layout_engine
        .calculate_layout(space, screen, &gaps, 0.0, Default::default(), Default::default())
        .iter()
        .any(|(layout_wid, _)| *layout_wid == wid)
}

type WindowUpdateTuple = (
    WindowId,
    Option<String>,
    Option<String>,
    Option<String>,
    bool,
    CGSize,
    Option<CGSize>,
    Option<CGSize>,
);

fn window_update_tuple(wid: WindowId) -> WindowUpdateTuple {
    (
        wid,
        None,
        None,
        None,
        true,
        CGSize::new(100.0, 100.0),
        None,
        None,
    )
}

struct TwoSpaceFixture {
    reactor: Reactor,
    screen1: CGRect,
    screen2: CGRect,
    space1: SpaceId,
    space2: SpaceId,
}

fn two_space_fixture() -> TwoSpaceFixture {
    let mut reactor = Reactor::new_for_test(LayoutEngine::new(
        &crate::common::config::VirtualWorkspaceSettings::default(),
        &crate::common::config::LayoutSettings::default(),
        None,
    ));
    let screen1 = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let screen2 = CGRect::new(CGPoint::new(1000., 0.), CGSize::new(1000., 1000.));
    let space1 = SpaceId::new(1);
    let space2 = SpaceId::new(2);

    reactor.handle_event(screen_params_event(
        vec![screen1, screen2],
        vec![Some(space1), Some(space2)],
        vec![],
    ));

    TwoSpaceFixture {
        reactor,
        screen1,
        screen2,
        space1,
        space2,
    }
}

// --- Display oscillation bug regression tests ---
//
// These tests cover the bug where a window enters a permanent oscillation state after a
// display topology change (e.g. MacBook lid open/close with an external monitor).  The
// root cause was that `sync_tiled_windows_for_app` could leave a window in two space
// layout trees simultaneously: after the window moved to the destination space its
// original source space still retained it, causing both spaces to issue conflicting
// SetWindowFrame calls that fed back into each other indefinitely.

#[test]
fn window_removed_from_source_space_when_dest_claims_it_first() {
    // Case 1: the destination space's WindowsOnScreenUpdated event fires before the
    // source space's empty event.  The VWM is updated by the destination event, so when
    // the source guard logic runs it can see that the window was moved away.
    let TwoSpaceFixture {
        mut reactor,
        screen1,
        screen2,
        space1,
        space2,
    } = two_space_fixture();
    let pid: pid_t = 42;
    let wid = WindowId::new(pid, 1);

    // Place window in space1's layout tree via a direct layout event.
    let _ = reactor
        .layout_manager
        .layout_engine
        .handle_event(LayoutEvent::WindowsOnScreenUpdated(
            space1,
            pid,
            vec![window_update_tuple(wid)],
            None,
        ));
    assert!(has_windows_in_layout(&mut reactor, space1, screen1, pid));

    // Destination space2 claims the window first (updates VWM: wid moves out of space1).
    let _ = reactor
        .layout_manager
        .layout_engine
        .handle_event(LayoutEvent::WindowsOnScreenUpdated(
            space2,
            pid,
            vec![window_update_tuple(wid)],
            None,
        ));

    // Source space1 receives the authoritative empty update.
    // Before the fix the guard in sync_tiled_windows_for_app checked only
    // has_windows_for_app (true) and skipped removal.  After the fix it also checks
    // whether those tree windows have been moved away in the VWM, and proceeds with
    // removal when they have.
    let _ = reactor
        .layout_manager
        .layout_engine
        .handle_event(LayoutEvent::WindowsOnScreenUpdated(space1, pid, vec![], None));

    assert!(
        !has_windows_in_layout(&mut reactor, space1, screen1, pid),
        "window must be removed from source space after destination claimed it"
    );
    assert!(
        has_windows_in_layout(&mut reactor, space2, screen2, pid),
        "window must remain in destination space"
    );
}

#[test]
fn empty_update_removes_window_when_vwm_was_preupdated() {
    // The reactor-level pre-pass in emit_layout_events updates the VWM for all claimed
    // windows upfront. This test mirrors that by updating the VWM directly before the
    // source's empty event.
    let TwoSpaceFixture {
        mut reactor,
        screen1,
        screen2,
        space1,
        space2,
    } = two_space_fixture();
    let pid: pid_t = 42;
    let wid = WindowId::new(pid, 1);

    let _ = reactor
        .layout_manager
        .layout_engine
        .handle_event(LayoutEvent::WindowsOnScreenUpdated(
            space1,
            pid,
            vec![window_update_tuple(wid)],
            None,
        ));
    assert!(has_windows_in_layout(&mut reactor, space1, screen1, pid));

    // Simulate the pre-pass: move wid from space1 to space2 in the VWM before any
    // per-space events fire.
    let space2_workspace = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager()
        .active_workspace(space2)
        .expect("space2 must have an active workspace");
    let (_assigned, destroyed) = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager_mut()
        .assign_window_to_workspace(space2, wid, space2_workspace);
    // wid is the lone window on space1's active workspace, which the
    // ephemeral guard refuses to destroy (active_anywhere check).
    debug_assert!(
        destroyed.is_empty(),
        "active source workspace should never be destroyed"
    );

    // Source space1's empty event fires first.  Because the VWM was pre-updated the
    // loop no longer re-adds wid to `desired`, so removal proceeds.
    let _ = reactor
        .layout_manager
        .layout_engine
        .handle_event(LayoutEvent::WindowsOnScreenUpdated(space1, pid, vec![], None));

    assert!(
        !has_windows_in_layout(&mut reactor, space1, screen1, pid),
        "window must be removed from source space when VWM was pre-updated (pre-pass scenario)"
    );

    // Destination space2 event fires after.
    let _ = reactor
        .layout_manager
        .layout_engine
        .handle_event(LayoutEvent::WindowsOnScreenUpdated(
            space2,
            pid,
            vec![window_update_tuple(wid)],
            None,
        ));
    assert!(has_windows_in_layout(&mut reactor, space2, screen2, pid));
}

#[test]
fn empty_update_only_removes_same_app_windows_moved_to_another_space() {
    // Mixed same-app case: one window moved to another space, while another window is
    // still assigned here but temporarily omitted from discovery. The empty update
    // should remove only the moved window from the source layout tree.
    let TwoSpaceFixture {
        mut reactor,
        screen1,
        screen2,
        space1,
        space2,
    } = two_space_fixture();
    let pid: pid_t = 42;
    let moved = WindowId::new(pid, 1);
    let retained = WindowId::new(pid, 2);

    let _ = reactor
        .layout_manager
        .layout_engine
        .handle_event(LayoutEvent::WindowsOnScreenUpdated(
            space1,
            pid,
            vec![window_update_tuple(moved), window_update_tuple(retained)],
            None,
        ));
    assert!(has_window_in_layout(&mut reactor, space1, screen1, moved));
    assert!(has_window_in_layout(&mut reactor, space1, screen1, retained));

    let space2_workspace = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager()
        .active_workspace(space2)
        .expect("space2 must have an active workspace");
    let (_assigned, destroyed) = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager_mut()
        .assign_window_to_workspace(space2, moved, space2_workspace);
    // `retained` keeps space1's active workspace populated, so even
    // setting `active_anywhere` aside there is no empty workspace for
    // the ephemeral guard to destroy.
    debug_assert!(
        destroyed.is_empty(),
        "source workspace still holds `retained`; no destruction expected"
    );

    let _ = reactor
        .layout_manager
        .layout_engine
        .handle_event(LayoutEvent::WindowsOnScreenUpdated(space1, pid, vec![], None));

    assert!(
        !has_window_in_layout(&mut reactor, space1, screen1, moved),
        "moved window must be removed from the source layout tree"
    );
    assert!(
        has_window_in_layout(&mut reactor, space1, screen1, retained),
        "same-app window still assigned to source space must be preserved"
    );

    let _ = reactor
        .layout_manager
        .layout_engine
        .handle_event(LayoutEvent::WindowsOnScreenUpdated(
            space2,
            pid,
            vec![window_update_tuple(moved)],
            None,
        ));
    assert!(has_window_in_layout(&mut reactor, space2, screen2, moved));
}

#[test]
fn window_preserved_in_space_on_empty_discovery_without_cross_space_move() {
    // Regression guard for the login-screen / AX-failure scenario: when the
    // accessibility API returns an empty window list but the window has NOT been moved
    // to another space in the VWM, the empty update must not destroy the layout.
    let mut reactor = Reactor::new_for_test(LayoutEngine::new(
        &crate::common::config::VirtualWorkspaceSettings::default(),
        &crate::common::config::LayoutSettings::default(),
        None,
    ));
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let space = SpaceId::new(1);
    let pid: pid_t = 42;
    let wid = WindowId::new(pid, 1);

    reactor.handle_event(screen_params_event(vec![screen], vec![Some(space)], vec![]));

    let _ = reactor
        .layout_manager
        .layout_engine
        .handle_event(LayoutEvent::WindowsOnScreenUpdated(
            space,
            pid,
            vec![window_update_tuple(wid)],
            None,
        ));
    assert!(has_windows_in_layout(&mut reactor, space, screen, pid));

    // AX returns empty — window is still in the VWM for this space (it was never moved).
    let _ = reactor
        .layout_manager
        .layout_engine
        .handle_event(LayoutEvent::WindowsOnScreenUpdated(space, pid, vec![], None));

    assert!(
        has_windows_in_layout(&mut reactor, space, screen, pid),
        "window must be preserved when empty update has no cross-space move (login screen / AX failure)"
    );
}

#[test]
fn discovery_after_display_change_places_window_on_correct_display() {
    // End-to-end integration test: a window that physically moved to a different
    // display after a topology change (lid open/close) must end up in only the new
    // display's layout tree, with no conflicting SetWindowFrame from the old one.
    //
    // This exercises the full WindowsDiscovered → emit_layout_events path including
    // the pre-pass VWM update (Case 2: source space processed first in screen order).
    let mut apps = Apps::new();
    let TwoSpaceFixture {
        mut reactor,
        screen1,
        screen2,
        space1,
        space2,
    } = two_space_fixture();

    // Window starts on screen1.
    reactor.handle_events(apps.make_app(1, make_windows(1)));
    apps.simulate_until_quiet(&mut reactor);
    assert_eq!(screen1, apps.windows[&WindowId::new(1, 1)].frame);

    // Simulate a topology change: the window has moved to screen2.
    // Passing it in `new` with an updated frame causes process_window_list to update
    // frame_monotonic so emit_layout_events assigns it to space2.
    // Note: without the fix this triggers the oscillation and simulate_until_quiet
    // would loop forever; the test itself documents that termination is part of the
    // expected behaviour.
    reactor.handle_event(Event::WindowsDiscovered {
        pid: 1,
        new: vec![(
            WindowId::new(1, 1),
            WindowInfo {
                frame: CGRect::new(CGPoint::new(1100., 100.), CGSize::new(50., 50.)),
                ..make_window(1)
            },
        )],
        known_visible: vec![WindowId::new(1, 1)],
    });
    apps.simulate_until_quiet(&mut reactor);

    assert!(
        !has_windows_in_layout(&mut reactor, space1, screen1, 1),
        "space1 layout tree must not contain the window after it moved to screen2"
    );
    assert!(
        has_windows_in_layout(&mut reactor, space2, screen2, 1),
        "space2 layout tree must contain the window after it moved to screen2"
    );
    assert_eq!(
        screen2,
        apps.windows[&WindowId::new(1, 1)].frame,
        "window must be laid out on screen2"
    );
}

#[test]
fn discovery_minimize_transition_removes_window_from_layout() {
    let mut apps = Apps::new();
    let mut reactor = Reactor::new_for_test(LayoutEngine::new(
        &crate::common::config::VirtualWorkspaceSettings::default(),
        &crate::common::config::LayoutSettings::default(),
        None,
    ));

    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let space = SpaceId::new(1);
    let wid = WindowId::new(1, 1);

    reactor.handle_event(screen_params_event(vec![screen], vec![Some(space)], vec![]));
    reactor.handle_events(apps.make_app(1, make_windows(1)));
    apps.simulate_until_quiet(&mut reactor);

    assert!(has_window_in_layout(&mut reactor, space, screen, wid));

    reactor.handle_event(Event::WindowsDiscovered {
        pid: 1,
        new: vec![(
            wid,
            WindowInfo {
                is_minimized: true,
                ..make_window(1)
            },
        )],
        known_visible: vec![],
    });

    assert!(
        !has_window_in_layout(&mut reactor, space, screen, wid),
        "minimized window must be removed from layout when discovery reports it minimized"
    );
    assert!(
        reactor
            .window_manager
            .window(wid)
            .is_some_and(|window| window.info.is_minimized),
        "reactor state must keep the window marked minimized"
    );
}

#[test]
fn switch_to_global_slot_routes_to_owning_display() {
    // Slots are populated lazily on first switch (creation-location semantics).
    // SwitchToGlobalSlot(N) must change the active workspace on whatever
    // display owns slot N, regardless of where the cursor is. The other space
    // stays put.
    let TwoSpaceFixture {
        mut reactor,
        screen1: _,
        screen2: _,
        space1,
        space2,
    } = two_space_fixture();

    // Lazy init runs during screen-params handling. Order is determined by
    // the screen iteration in `expose_all_spaces`, which runs before the
    // display-UUID mirror is fully populated; the practical outcome is that
    // space2 gets workspace number 0 and space1 gets number 1. The exact
    // numbers don't matter for what we're testing — we just need to know
    // which space owns slot N before pressing Cmd+N.
    let (owning_space, owning_slot) = {
        let vwm = reactor.layout_manager.layout_engine.virtual_workspace_manager();
        let target = vwm.resolve_workspace(0).expect("slot 0 should resolve after lazy init");
        (target.space, 0usize)
    };
    let other_space = if owning_space == space1 {
        space2
    } else {
        space1
    };
    let owning_uuid = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager()
        .space_display(owning_space)
        .expect("owning space has a display uuid")
        .to_owned();

    // Move the owning space off the slot so SwitchToGlobalSlot has somewhere
    // observable to switch back to. Pre-Phase-3 this happened automatically
    // because lazy init produced four workspaces per space.
    let parking_ws = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager_mut()
        .create_workspace_with_number(7, &owning_uuid, owning_space);
    reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager_mut()
        .set_active_workspace(owning_space, parking_ws);

    let slot_target = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager()
        .resolve_workspace(owning_slot)
        .expect("slot should still resolve to its owning space");
    let slot_ws = slot_target.workspace_id;
    assert_eq!(slot_target.space, owning_space);

    let initial_owning = reactor.layout_manager.layout_engine.active_workspace(owning_space);
    let initial_other = reactor.layout_manager.layout_engine.active_workspace(other_space);

    reactor.handle_event(Event::Command(Command::Layout(
        LayoutCommand::SwitchToGlobalSlot(owning_slot),
    )));

    let after_owning = reactor.layout_manager.layout_engine.active_workspace(owning_space);
    let after_other = reactor.layout_manager.layout_engine.active_workspace(other_space);

    assert_eq!(
        after_owning,
        Some(slot_ws),
        "the slot's workspace should now be active on its owning space"
    );
    assert_ne!(
        initial_owning, after_owning,
        "owning space's active workspace must have changed"
    );
    assert_eq!(
        after_other, initial_other,
        "non-owning space's active workspace must be unaffected by the global-slot switch"
    );
}

#[test]
fn discovery_restore_transition_readds_window_to_layout() {
    let mut apps = Apps::new();
    let mut reactor = Reactor::new_for_test(LayoutEngine::new(
        &crate::common::config::VirtualWorkspaceSettings::default(),
        &crate::common::config::LayoutSettings::default(),
        None,
    ));

    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let space = SpaceId::new(1);
    let wid = WindowId::new(1, 1);
    let mut windows = make_windows(1);
    windows[0].is_minimized = true;

    reactor.handle_event(screen_params_event(vec![screen], vec![Some(space)], vec![]));
    reactor.handle_events(apps.make_app(1, windows));
    apps.simulate_until_quiet(&mut reactor);

    assert!(
        !has_window_in_layout(&mut reactor, space, screen, wid),
        "startup-minimized window must not be inserted into layout"
    );

    reactor.handle_event(Event::WindowsDiscovered {
        pid: 1,
        new: vec![(wid, make_window(1))],
        known_visible: vec![wid],
    });

    assert!(
        has_window_in_layout(&mut reactor, space, screen, wid),
        "restored window must return to layout when discovery reports it visible again"
    );
    assert!(
        reactor
            .window_manager
            .window(wid)
            .is_some_and(|window| !window.info.is_minimized),
        "reactor state must clear the minimized flag after restore"
    );
}

#[test]
fn unfullscreen_restores_window_tracking() {
    let mut apps = Apps::new();
    let mut reactor = Reactor::new_for_test(LayoutEngine::new(
        &crate::common::config::VirtualWorkspaceSettings::default(),
        &crate::common::config::LayoutSettings::default(),
        None,
    ));

    let user_space = SpaceId::new(1);
    let fullscreen_space = SpaceId::new(0x400000000 + user_space.get());
    let full_screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));

    // Set up a display with a user space and some windows.
    reactor.handle_event(screen_params_event(
        vec![full_screen],
        vec![Some(user_space)],
        vec![],
    ));
    reactor.handle_events(apps.make_app_with_opts(
        1,
        make_windows(1),
        Some(WindowId::new(1, 1)),
        true,
        true,
    ));
    reactor.handle_event(Event::ApplicationGloballyActivated(1));
    apps.simulate_until_quiet(&mut reactor);

    // Record the window as fullscreened.
    let window_id = WindowId::new(1, 1);
    reactor.space_manager.fullscreen_by_space.insert(
        fullscreen_space.get(),
        FullscreenSpaceTrack {
            windows: vec![FullscreenWindowTrack {
                pid: 1,
                window_id: Some(window_id),
                last_known_user_space: Some(user_space),
                _last_seen_fullscreen_space: fullscreen_space,
            }],
        },
    );

    // Transition to fullscreen space.
    reactor.handle_event(Event::SpaceChanged(vec![Some(fullscreen_space)]));
    apps.simulate_until_quiet(&mut reactor);

    // Exit fullscreen (return to user space).
    reactor.handle_event(Event::SpaceChanged(vec![Some(user_space)]));

    // The reactor should trigger a GetVisibleWindows request.
    let mut saw_get_visible_windows = false;
    for request in apps.requests() {
        if matches!(request, Request::GetVisibleWindows) {
            saw_get_visible_windows = true;
        }
    }
    assert!(
        saw_get_visible_windows,
        "Should send GetVisibleWindows to app on unfullscreen"
    );

    // The fullscreen track should be removed.
    assert!(
        !reactor.space_manager.fullscreen_by_space.contains_key(&fullscreen_space.get()),
        "Fullscreen track should be removed from space manager"
    );
}

#[test]
fn switch_to_global_slot_cross_display_skips_old_target_workspace_focus() {
    let TwoSpaceFixture {
        mut reactor,
        screen1,
        screen2,
        space1,
        space2,
    } = two_space_fixture();
    let (raise_manager_tx, mut raise_manager_rx) = actor::channel();
    reactor.communication_manager.raise_manager_tx = raise_manager_tx;

    let _ = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager_mut()
        .list_workspaces(space1);
    let _ = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager_mut()
        .list_workspaces(space2);

    let mut apps = Apps::new();
    let mut windows = make_windows(2);
    windows[0].frame.origin = CGPoint::new(screen1.origin.x + 100.0, screen1.origin.y + 100.0);
    windows[1].frame.origin = CGPoint::new(screen2.origin.x + 100.0, screen2.origin.y + 100.0);
    reactor.handle_events(apps.make_app(70, windows));
    apps.simulate_until_quiet(&mut reactor);
    while raise_manager_rx.try_recv().is_ok() {}

    let uuid1 = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager()
        .space_display(space1)
        .expect("space1 has a display uuid")
        .to_owned();
    let uuid2 = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager()
        .space_display(space2)
        .expect("space2 has a display uuid")
        .to_owned();
    let ws8 = reactor.layout_manager.layout_engine.create_workspace_on_display(
        8,
        &uuid1,
        space1,
        screen1.size,
    );
    let ws9 = reactor.layout_manager.layout_engine.create_workspace_on_display(
        9,
        &uuid2,
        space2,
        screen2.size,
    );

    let source_space = reactor.workspace_command_space().unwrap_or(space1);
    let (target_space, target_slot, old_target_window) = if source_space == space1 {
        (space2, 9, WindowId::new(70, 2))
    } else {
        (space1, 8, WindowId::new(70, 1))
    };
    let expected_ws = if target_space == space1 { ws8 } else { ws9 };
    assert_ne!(
        reactor.layout_manager.layout_engine.active_workspace(target_space),
        Some(expected_ws),
        "precondition: target display must start on a different workspace"
    );

    reactor.handle_event(Event::Command(Command::Layout(
        LayoutCommand::SwitchToGlobalSlot(target_slot),
    )));

    assert_eq!(
        reactor.layout_manager.layout_engine.active_workspace(target_space),
        Some(expected_ws),
        "cross-display global slot switch must activate the target workspace immediately"
    );

    let mut focused_windows = Vec::new();
    while let Ok((_, msg)) = raise_manager_rx.try_recv() {
        if let raise_manager::Event::RaiseRequest(RaiseRequest {
            focus_window: Some((wid, _)),
            ..
        }) = msg
        {
            focused_windows.push(wid);
        }
    }

    assert!(
        !focused_windows.contains(&old_target_window),
        "cross-display switch must not focus the target display's old active workspace first"
    );
}

#[test]
fn switch_to_global_slot_survives_display_replug() {
    // Reproduces the user-reported bug: after a display unplug + replug
    // (which gives the display a fresh SpaceId), Cmd+1 must still land on
    // the secondary display when slot 1 is pinned to its UUID.
    let mut settings = crate::common::config::VirtualWorkspaceSettings::default();
    settings.display_default_workspaces.insert("test-display-1".to_string(), 1);
    let mut reactor = Reactor::new_for_test(LayoutEngine::new(
        &settings,
        &crate::common::config::LayoutSettings::default(),
        None,
    ));
    let screen1 = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let screen2 = CGRect::new(CGPoint::new(1000., 0.), CGSize::new(1000., 1000.));
    let space1 = SpaceId::new(1);
    let original_space2 = SpaceId::new(2);

    reactor.handle_event(screen_params_event(
        vec![screen1, screen2],
        vec![Some(space1), Some(original_space2)],
        vec![],
    ));

    let initial_slot = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager()
        .resolve_workspace(1)
        .expect("pinned slot 1 should resolve on startup");
    assert_eq!(initial_slot.space, original_space2);

    // Unplug the secondary display. Its workspaces are migrated/destroyed,
    // freeing workspace number 1 for a fresh default on replug.
    reactor.handle_event(screen_params_event(vec![screen1], vec![Some(space1)], vec![]));

    // Replug the same display UUID with a fresh SpaceId.
    let new_space2 = SpaceId::new(999);
    reactor.handle_event(screen_params_event(
        vec![screen1, screen2],
        vec![Some(space1), Some(new_space2)],
        vec![],
    ));

    // Press Cmd+1 from screen1 (the non-pinned display).
    reactor.handle_event(Event::Command(Command::Layout(
        LayoutCommand::SwitchToGlobalSlot(1),
    )));

    // Slot 1 should be active on screen2's NEW SpaceId, not screen1. The
    // workspace_id is preserved across the replug because remap_space rewires
    // the SpaceId on the existing entry.
    let slot_target = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager()
        .resolve_workspace(1)
        .expect("slot 1 should resolve after replug");
    assert_eq!(
        slot_target.space, new_space2,
        "slot 1 must live on the pinned display's new SpaceId after replug"
    );

    let new_space2_active = reactor.layout_manager.layout_engine.active_workspace(new_space2);
    assert_eq!(
        new_space2_active,
        Some(slot_target.workspace_id),
        "slot 1 must land on the pinned display after replug"
    );
}

#[test]
fn switch_to_global_slot_creates_workspace_when_absent() {
    // Cmd+N for a workspace number that doesn't exist creates it on the
    // focused display, then activates it.
    let TwoSpaceFixture {
        mut reactor,
        screen1: _,
        screen2: _,
        space1,
        space2,
    } = two_space_fixture();

    // Force lazy init so each space gets its default workspace.
    let _ = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager_mut()
        .list_workspaces(space1);
    let _ = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager_mut()
        .list_workspaces(space2);

    // Slot 7 is well outside what lazy init would pick (smallest-unused gives
    // 0 and 1 to the two spaces).
    const SLOT: usize = 7;
    assert!(
        reactor
            .layout_manager
            .layout_engine
            .virtual_workspace_manager()
            .resolve_workspace(SLOT)
            .is_none(),
        "precondition: slot {} must not exist before Cmd+{}",
        SLOT,
        SLOT,
    );

    reactor.handle_event(Event::Command(Command::Layout(
        LayoutCommand::SwitchToGlobalSlot(SLOT),
    )));

    let target = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager()
        .resolve_workspace(SLOT)
        .expect("slot must be created on demand");
    let active = reactor.layout_manager.layout_engine.active_workspace(target.space);
    assert_eq!(
        active,
        Some(target.workspace_id),
        "newly created slot must be active"
    );
}

#[test]
fn switch_to_global_slot_repeats_active_slot_when_back_and_forth_enabled() {
    let mut settings = crate::common::config::VirtualWorkspaceSettings::default();
    settings.workspace_auto_back_and_forth = true;
    let mut reactor = Reactor::new_for_test(LayoutEngine::new(
        &settings,
        &crate::common::config::LayoutSettings::default(),
        None,
    ));
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let space = SpaceId::new(1);

    reactor.handle_event(screen_params_event(vec![screen], vec![Some(space)], vec![]));

    let default_ws = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager_mut()
        .list_workspaces(space)
        .first()
        .map(|(id, _)| *id)
        .expect("space should lazily initialize a default workspace");
    let uuid = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager()
        .space_display(space)
        .expect("space has a display uuid")
        .to_owned();
    let ws7 = reactor.layout_manager.layout_engine.create_workspace_on_display(
        7,
        &uuid,
        space,
        screen.size,
    );

    assert!(reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager_mut()
        .set_active_workspace(space, ws7));
    assert_eq!(
        reactor
            .layout_manager
            .layout_engine
            .virtual_workspace_manager()
            .last_workspace(space),
        Some(default_ws),
        "precondition: default workspace must be recorded as last workspace"
    );

    reactor.handle_event(Event::Command(Command::Layout(
        LayoutCommand::SwitchToGlobalSlot(7),
    )));

    assert_eq!(
        reactor.layout_manager.layout_engine.active_workspace(space),
        Some(default_ws),
        "repeating the active global slot should switch back to the last workspace"
    );
}

#[test]
fn empty_workspace_destroyed_unless_active() {
    // Setup: two-space fixture, window in ws 7 (on space1), space1 active on a
    // different workspace. Closing the window must destroy ws 7 because it is
    // empty AND not active anywhere.
    //
    // We drive the workspace creation and window assignment directly through
    // the VWM API rather than going through SwitchToGlobalSlot + make_app:
    // SwitchToGlobalSlot's "create on focused display" fallback consults the
    // real cursor position via SLSGetCurrentCursorLocation (no test
    // override), so the destination display is environment-dependent. Direct
    // VWM mutation pins ws 7 to space1 deterministically. The destruction
    // path under test (Event::WindowDestroyed → LayoutEvent::WindowRemoved →
    // VWM::remove_window → destroy_workspace_if_ephemeral) is exercised in
    // full once we send the WindowDestroyed event.
    let TwoSpaceFixture {
        mut reactor, space1, space2, ..
    } = two_space_fixture();
    let _ = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager_mut()
        .list_workspaces(space1);
    let _ = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager_mut()
        .list_workspaces(space2);

    // Create ws 7 on space1 explicitly, then activate it so the upcoming
    // window discovery puts the window there.
    let space1_uuid = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager()
        .space_display(space1)
        .expect("space1 has a display uuid")
        .to_owned();
    let ws7 = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager_mut()
        .create_workspace_with_number(7, &space1_uuid, space1);
    let other_ws_on_space1 = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager_mut()
        .list_workspaces(space1)
        .into_iter()
        .map(|(id, _)| id)
        .find(|id| *id != ws7)
        .expect("space1 has a non-ws-7 workspace from lazy init");
    assert!(
        reactor
            .layout_manager
            .layout_engine
            .virtual_workspace_manager_mut()
            .set_active_workspace(space1, ws7)
    );

    // Register window 99 with the reactor, then explicitly pin it to ws 7
    // (the discovery flow's choice of space depends on best_space_for_window,
    // which is brittle in tests; this keeps the assignment deterministic).
    let mut apps = Apps::new();
    reactor.handle_events(apps.make_app(99, make_windows(1)));
    let win = WindowId::new(99, 1);
    assert!(
        reactor
            .layout_manager
            .layout_engine
            .virtual_workspace_manager_mut()
            .assign_window_to_workspace(space1, win, ws7)
            .0,
        "precondition: window must be assignable to ws 7"
    );
    assert_eq!(
        reactor
            .layout_manager
            .layout_engine
            .virtual_workspace_manager()
            .workspace_for_window(win),
        Some(ws7),
        "precondition: window must be in ws 7 before destruction"
    );

    // Move space1's active workspace away from ws 7 so ws 7 is no longer
    // active on any display.
    assert!(
        reactor
            .layout_manager
            .layout_engine
            .virtual_workspace_manager_mut()
            .set_active_workspace(space1, other_ws_on_space1)
    );
    // Precondition: ws 7 must not be the active workspace on any display.
    // In the new model `active_workspace` is per-display, so we check that
    // walking every initialized space's active workspace yields no `ws7`.
    {
        let mgr = reactor.layout_manager.layout_engine.virtual_workspace_manager();
        let initialized: Vec<_> = mgr.initialized_spaces();
        for sp in initialized {
            assert_ne!(
                mgr.active_workspace(sp),
                Some(ws7),
                "precondition: ws 7 must not be active on space {:?} before destruction",
                sp
            );
        }
    }

    reactor.handle_event(Event::WindowDestroyed(win));

    assert!(
        reactor
            .layout_manager
            .layout_engine
            .virtual_workspace_manager()
            .resolve_workspace(7)
            .is_none(),
        "ws 7 should be destroyed after last window removed and not active"
    );
}

// Regression for Critical #1: rebalance_all_layouts panics on dead workspace
// id when the workspace is destroyed via Event::WindowDestroyed and a
// `workspace_layouts` entry was created for it. Reproduces the realistic
// flow that crashes pre-fix: `LayoutEngine::create_workspace_on_display`
// creates BOTH the workspace AND the workspace_layouts entry, so the
// destroy_workspace_if_ephemeral path leaves a dangling
// (SpaceId, dead_ws_id) entry in workspace_layouts. The next
// `rebalance_all_layouts` iterates that entry and feeds the dead id into
// `workspace_tree_mut` (a direct SlotMap index → panic).
#[test]
fn empty_workspace_destroyed_via_window_destroyed_through_engine() {
    let TwoSpaceFixture {
        mut reactor,
        screen1,
        space1,
        space2,
        ..
    } = two_space_fixture();

    // Lazy-init both spaces so each has a default workspace.
    let _ = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager_mut()
        .list_workspaces(space1);
    let _ = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager_mut()
        .list_workspaces(space2);

    let space1_uuid = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager()
        .space_display(space1)
        .expect("space1 has a display uuid")
        .to_owned();

    // Create ws 7 via the LayoutEngine API used by SwitchToGlobalSlot — this
    // is the path that ALSO creates a `workspace_layouts` entry for ws 7
    // (via ensure_active_for_workspace). Without this entry, the panic
    // doesn't fire (and the existing test masks the bug).
    let ws7 = reactor.layout_manager.layout_engine.create_workspace_on_display(
        7,
        &space1_uuid,
        space1,
        screen1.size,
    );

    // Pick a non-ws-7 workspace on space1 to switch active to later.
    let other_ws_on_space1 = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager_mut()
        .list_workspaces(space1)
        .into_iter()
        .map(|(id, _)| id)
        .find(|id| *id != ws7)
        .expect("space1 has a non-ws-7 workspace from lazy init");
    assert!(
        reactor
            .layout_manager
            .layout_engine
            .virtual_workspace_manager_mut()
            .set_active_workspace(space1, ws7)
    );

    // Register a window — discovery will auto-assign it to the active
    // workspace (ws 7), and add_window_to_layout will tile it because
    // workspace_layouts.active(space1, ws7) is now Some.
    let mut apps = Apps::new();
    reactor.handle_events(apps.make_app(99, make_windows(1)));
    let win = WindowId::new(99, 1);
    assert_eq!(
        reactor
            .layout_manager
            .layout_engine
            .virtual_workspace_manager()
            .workspace_for_window(win),
        Some(ws7),
        "precondition: window must be in ws 7 (active workspace) after discovery"
    );

    // Switch active away so ws 7 becomes destruction-eligible.
    assert!(
        reactor
            .layout_manager
            .layout_engine
            .virtual_workspace_manager_mut()
            .set_active_workspace(space1, other_ws_on_space1)
    );

    // The destruction path: WindowDestroyed → WindowRemoved → remove_window_internal
    // → vwm.remove_window (destroys ws 7) → rebalance_all_layouts (panics
    // pre-fix because workspace_layouts still has the dead entry).
    reactor.handle_event(Event::WindowDestroyed(win));

    assert!(
        reactor
            .layout_manager
            .layout_engine
            .virtual_workspace_manager()
            .resolve_workspace(7)
            .is_none(),
        "ws 7 should be destroyed after last window removed and not active"
    );
}

// Regression for Critical #1 via the move-window path:
// `MoveWindowToWorkspace` calls VWM::assign_window_to_workspace which can
// destroy the source workspace (when it becomes empty + not active). This
// test exercises the destruction directly via the VWM API. The engine-side
// `workspace_layouts` cleanup is exercised indirectly: any production
// caller of `assign_window_to_workspace` (engine.rs MoveWindowToWorkspace,
// move_window_to_space, the app-rule re-assign in reactor.rs) takes the
// returned destroyed list and calls `drop_workspace_layout` — so a
// destroyed source can never be re-touched by `rebalance_all_layouts`.
// (Test 1 above is the live-engine regression for that mirror cleanup.)
#[test]
fn empty_workspace_destroyed_when_window_moved_away() {
    let TwoSpaceFixture {
        mut reactor,
        screen1,
        space1,
        space2,
        ..
    } = two_space_fixture();

    let _ = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager_mut()
        .list_workspaces(space1);
    let _ = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager_mut()
        .list_workspaces(space2);

    let space1_uuid = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager()
        .space_display(space1)
        .expect("space1 has a display uuid")
        .to_owned();

    // Create ws 7 with a workspace_layouts entry, set active so the new
    // window lands there.
    let ws7 = reactor.layout_manager.layout_engine.create_workspace_on_display(
        7,
        &space1_uuid,
        space1,
        screen1.size,
    );
    let other_ws_on_space1 = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager_mut()
        .list_workspaces(space1)
        .into_iter()
        .map(|(id, _)| id)
        .find(|id| *id != ws7)
        .expect("space1 has a non-ws-7 workspace from lazy init");
    assert!(
        reactor
            .layout_manager
            .layout_engine
            .virtual_workspace_manager_mut()
            .set_active_workspace(space1, ws7)
    );

    let mut apps = Apps::new();
    reactor.handle_events(apps.make_app(99, make_windows(1)));
    let win = WindowId::new(99, 1);
    assert_eq!(
        reactor
            .layout_manager
            .layout_engine
            .virtual_workspace_manager()
            .workspace_for_window(win),
        Some(ws7),
        "precondition: window must be in ws 7"
    );

    // Switch active away so ws 7 becomes empty AND non-active when we move
    // the window.
    assert!(
        reactor
            .layout_manager
            .layout_engine
            .virtual_workspace_manager_mut()
            .set_active_workspace(space1, other_ws_on_space1)
    );

    // Move the window to other_ws_on_space1. The assign returns
    // `(success, destroyed)` — destroyed must contain (space1, ws7).
    let (assigned, destroyed) = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager_mut()
        .assign_window_to_workspace(space1, win, other_ws_on_space1);
    assert!(assigned, "moving window to another workspace must succeed");
    assert!(
        destroyed.iter().any(|(sp, ws_id)| *sp == space1 && *ws_id == ws7),
        "destroyed list must include (space1, ws7)"
    );

    assert!(
        reactor
            .layout_manager
            .layout_engine
            .virtual_workspace_manager()
            .resolve_workspace(7)
            .is_none(),
        "ws 7 should be destroyed after window moved away"
    );
}

// Regression for Critical #2: destroy_workspace must scrub the last_active
// slot in active_workspace_per_space, otherwise `last_workspace(space)`
// returns a dangling id and SwitchToLastWorkspace silently no-ops.
#[test]
fn last_active_cleared_when_destroyed() {
    let TwoSpaceFixture {
        mut reactor,
        screen1,
        space1,
        space2,
        ..
    } = two_space_fixture();

    let _ = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager_mut()
        .list_workspaces(space1);
    let _ = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager_mut()
        .list_workspaces(space2);

    let space1_uuid = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager()
        .space_display(space1)
        .expect("space1 has a display uuid")
        .to_owned();

    // Create ws 7 with a layouts entry; we'll make it active, place a window,
    // switch active away (so ws 7 becomes the `last_active`), then drop the
    // window.
    let ws7 = reactor.layout_manager.layout_engine.create_workspace_on_display(
        7,
        &space1_uuid,
        space1,
        screen1.size,
    );
    let other_ws_on_space1 = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager_mut()
        .list_workspaces(space1)
        .into_iter()
        .map(|(id, _)| id)
        .find(|id| *id != ws7)
        .expect("space1 has a non-ws-7 workspace from lazy init");

    assert!(
        reactor
            .layout_manager
            .layout_engine
            .virtual_workspace_manager_mut()
            .set_active_workspace(space1, ws7)
    );

    let mut apps = Apps::new();
    reactor.handle_events(apps.make_app(99, make_windows(1)));
    let win = WindowId::new(99, 1);
    assert_eq!(
        reactor
            .layout_manager
            .layout_engine
            .virtual_workspace_manager()
            .workspace_for_window(win),
        Some(ws7),
        "precondition: window must be in ws 7"
    );

    // Switch away — ws 7 becomes `last_active` for space1.
    assert!(
        reactor
            .layout_manager
            .layout_engine
            .virtual_workspace_manager_mut()
            .set_active_workspace(space1, other_ws_on_space1)
    );
    assert_eq!(
        reactor
            .layout_manager
            .layout_engine
            .virtual_workspace_manager()
            .last_workspace(space1),
        Some(ws7),
        "precondition: ws 7 must be the last_active for space1"
    );

    // Destroy the window — ws 7 destruction follows.
    reactor.handle_event(Event::WindowDestroyed(win));

    // last_workspace must NOT return the dead id.
    assert!(
        reactor
            .layout_manager
            .layout_engine
            .virtual_workspace_manager()
            .last_workspace(space1)
            != Some(ws7),
        "last_workspace must not return the destroyed ws 7"
    );
    // And the workspace itself is gone.
    assert!(
        reactor
            .layout_manager
            .layout_engine
            .virtual_workspace_manager()
            .resolve_workspace(7)
            .is_none(),
        "ws 7 should be destroyed"
    );
}

// Task 3.3: When the user dispatches `MoveWindowToWorkspace { workspace: N }`
// and slot N has no workspace anywhere, the engine creates a workspace
// numbered N on the SOURCE display (the display owning the moved window) and
// moves the window into it. Symmetric to SwitchToGlobalSlot create-on-demand
// (Task 3.1b), so Cmd+N and Cmd+Shift+N agree on which display owns slot N.
#[test]
fn move_window_to_workspace_creates_target_on_source_display() {
    let TwoSpaceFixture { mut reactor, space1, .. } = two_space_fixture();
    // Lazy-init space1's default workspace (also fired by the fixture's
    // ScreenParametersChanged handling, but explicit here mirrors the plan).
    let _ = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager_mut()
        .list_workspaces(space1);
    let mut apps = Apps::new();
    reactor.handle_events(apps.make_app(50, make_windows(1)));
    let win = WindowId::new(50, 1);

    // ws 4 doesn't exist anywhere yet.
    assert!(
        reactor
            .layout_manager
            .layout_engine
            .virtual_workspace_manager()
            .resolve_workspace(4)
            .is_none(),
        "precondition: ws 4 must not exist"
    );

    // Move the window to ws 4.
    reactor.handle_event(Event::Command(Command::Layout(
        LayoutCommand::MoveWindowToWorkspace {
            workspace: 4,
            window_id: Some(win.idx.get()),
        },
    )));

    let target = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager()
        .resolve_workspace(4)
        .expect("ws 4 should be created on the source display");
    assert_eq!(
        target.space, space1,
        "ws 4 should be on the source display (space1)"
    );

    // The window must actually be in ws 4 now (not just that ws 4 exists).
    let ws_for_win = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager()
        .workspace_for_window(win)
        .expect("window should still be tracked on space1");
    assert_eq!(
        ws_for_win, target.workspace_id,
        "window should be moved into ws 4"
    );
}

#[test]
fn empty_workspace_destroyed_when_switching_away_and_fallback_exists() {
    let TwoSpaceFixture {
        mut reactor, screen1, space1, ..
    } = two_space_fixture();
    let _ = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager_mut()
        .list_workspaces(space1);

    let space1_uuid = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager()
        .space_display(space1)
        .expect("space1 has a display uuid")
        .to_owned();
    let ws7 = reactor.layout_manager.layout_engine.create_workspace_on_display(
        7,
        &space1_uuid,
        space1,
        screen1.size,
    );
    assert!(
        reactor
            .layout_manager
            .layout_engine
            .virtual_workspace_manager_mut()
            .set_active_workspace(space1, ws7)
    );

    let _ = reactor
        .layout_manager
        .layout_engine
        .handle_virtual_workspace_command(space1, &LayoutCommand::SwitchToWorkspace(0));

    assert!(
        reactor
            .layout_manager
            .layout_engine
            .virtual_workspace_manager()
            .resolve_workspace(7)
            .is_none(),
        "empty ws 7 should be destroyed after switching away"
    );
}

// Task 3.3 + 3.2: when the source workspace becomes empty, it is
// destroyed even if it was active, as long as the display has another
// workspace to fall back to. Each display keeps at least one workspace, but
// empty extras should not linger.
#[test]
fn move_window_to_workspace_destroys_active_empty_source_when_fallback_exists() {
    let TwoSpaceFixture { mut reactor, space1, .. } = two_space_fixture();
    let _ = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager_mut()
        .list_workspaces(space1);
    let mut apps = Apps::new();
    reactor.handle_events(apps.make_app(50, make_windows(1)));
    let win = WindowId::new(50, 1);

    let source_ws = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager()
        .workspace_for_window(win)
        .expect("window must be in some source workspace");
    assert_eq!(
        reactor
            .layout_manager
            .layout_engine
            .virtual_workspace_manager()
            .active_workspace(space1),
        Some(source_ws),
        "precondition: source ws is active on space1",
    );

    reactor.handle_event(Event::Command(Command::Layout(
        LayoutCommand::MoveWindowToWorkspace {
            workspace: 4,
            window_id: Some(win.idx.get()),
        },
    )));

    let target = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager()
        .resolve_workspace(4)
        .expect("ws 4 must be created by the move");
    assert_eq!(
        reactor
            .layout_manager
            .layout_engine
            .virtual_workspace_manager()
            .workspace_for_window(win),
        Some(target.workspace_id),
        "window must actually be moved into ws 4",
    );

    assert!(
        reactor
            .layout_manager
            .layout_engine
            .virtual_workspace_manager()
            .workspace_space(source_ws)
            .is_none(),
        "empty source workspace should be destroyed once ws 4 can keep the display alive",
    );
    assert_eq!(
        reactor
            .layout_manager
            .layout_engine
            .virtual_workspace_manager()
            .active_workspace(space1),
        Some(target.workspace_id),
        "display active workspace should fall back to the remaining ws 4",
    );
}

#[test]
fn move_focused_window_from_ws1_to_new_workspace_activates_target() {
    let mut reactor = Reactor::new_for_test(LayoutEngine::new(
        &crate::common::config::VirtualWorkspaceSettings::default(),
        &crate::common::config::LayoutSettings::default(),
        None,
    ));
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let space = SpaceId::new(1);
    reactor.handle_event(screen_params_event(vec![screen], vec![Some(space)], vec![]));

    // Initialize the default ws0 first, then create/switch to ws1. This
    // mirrors the user's reported source workspace while leaving ws0 as the
    // previous workspace fallback.
    let _ = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager_mut()
        .list_workspaces(space);
    let display_uuid = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager()
        .space_display(space)
        .expect("space has a display uuid")
        .to_owned();
    let ws1 = reactor.layout_manager.layout_engine.create_workspace_on_display(
        1,
        &display_uuid,
        space,
        screen.size,
    );
    assert!(
        reactor
            .layout_manager
            .layout_engine
            .virtual_workspace_manager_mut()
            .set_active_workspace(space, ws1)
    );

    let mut apps = Apps::new();
    reactor.handle_events(apps.make_app(50, make_windows(1)));
    let win = WindowId::new(50, 1);
    reactor.send_layout_event(LayoutEvent::WindowFocused(space, win));

    assert_eq!(
        reactor
            .layout_manager
            .layout_engine
            .virtual_workspace_manager()
            .workspace_for_window(win),
        Some(ws1),
        "precondition: focused window starts on ws1",
    );

    reactor.handle_event(Event::Command(Command::Layout(
        LayoutCommand::MoveWindowToWorkspace { workspace: 2, window_id: None },
    )));

    let target = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager()
        .resolve_workspace(2)
        .expect("ws2 must be created by the move");
    assert_eq!(
        reactor
            .layout_manager
            .layout_engine
            .virtual_workspace_manager()
            .workspace_for_window(win),
        Some(target.workspace_id),
        "window must move from ws1 into ws2",
    );
    assert_eq!(
        reactor
            .layout_manager
            .layout_engine
            .virtual_workspace_manager()
            .active_workspace(space),
        Some(target.workspace_id),
        "moving the only focused window out of active ws1 should make ws2 active, not fall back to empty ws0",
    );
}

// Task 3.3 + 3.2: When the source workspace is BOTH empty AND non-active
// after the move, the Phase 3.2 ephemeral path destroys it through the
// MoveWindowToWorkspace handler. Verifies the destruction and that the
// engine-side `workspace_layouts` mirror is also cleaned up (no panic on
// next rebalance).
#[test]
fn move_window_to_workspace_destroys_empty_inactive_source() {
    let TwoSpaceFixture {
        mut reactor, screen1, space1, ..
    } = two_space_fixture();
    let _ = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager_mut()
        .list_workspaces(space1);

    let space1_uuid = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager()
        .space_display(space1)
        .expect("space1 has a display uuid")
        .to_owned();

    // Create ws 5 explicitly (so it has a workspace_layouts entry); make it
    // active so the discovered window lands there.
    let ws5 = reactor.layout_manager.layout_engine.create_workspace_on_display(
        5,
        &space1_uuid,
        space1,
        screen1.size,
    );
    assert!(
        reactor
            .layout_manager
            .layout_engine
            .virtual_workspace_manager_mut()
            .set_active_workspace(space1, ws5)
    );

    let mut apps = Apps::new();
    reactor.handle_events(apps.make_app(50, make_windows(1)));
    let win = WindowId::new(50, 1);
    assert_eq!(
        reactor
            .layout_manager
            .layout_engine
            .virtual_workspace_manager()
            .workspace_for_window(win),
        Some(ws5),
        "precondition: window in ws 5 (active)",
    );

    // Switch active away — ws 5 is non-active, still holds the window.
    let other_ws = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager_mut()
        .list_workspaces(space1)
        .into_iter()
        .map(|(id, _)| id)
        .find(|id| *id != ws5)
        .expect("space1 has another workspace from lazy init");
    assert!(
        reactor
            .layout_manager
            .layout_engine
            .virtual_workspace_manager_mut()
            .set_active_workspace(space1, other_ws)
    );

    // Move the window to (newly created) ws 4. Source ws 5 → empty AND
    // non-active → destroyed.
    reactor.handle_event(Event::Command(Command::Layout(
        LayoutCommand::MoveWindowToWorkspace {
            workspace: 4,
            window_id: Some(win.idx.get()),
        },
    )));

    // ws 5 must be destroyed (number 5 no longer resolves anywhere).
    assert!(
        reactor
            .layout_manager
            .layout_engine
            .virtual_workspace_manager()
            .resolve_workspace(5)
            .is_none(),
        "ws 5 must be destroyed after window leaves AND it's non-active",
    );

    // Window must have moved into ws 4 (the newly created target).
    let target = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager()
        .resolve_workspace(4)
        .expect("ws 4 should have been created on the source display");
    assert_eq!(target.space, space1, "ws 4 should be on space1");
    assert_eq!(
        reactor
            .layout_manager
            .layout_engine
            .virtual_workspace_manager()
            .workspace_for_window(win),
        Some(target.workspace_id),
        "window should be in ws 4",
    );
}

// Regression: a hotkey can target a newly focused window before discovery has
// assigned it to a virtual workspace. The pending target must guide first
// discovery and the immediate stale app refresh that follows a space switch.
#[test]
fn pending_current_window_move_survives_discovery_race() {
    let mut settings = crate::common::config::VirtualWorkspaceSettings::default();
    settings.display_default_workspaces.insert("test-display-0".into(), 1);
    settings.display_default_workspaces.insert("test-display-1".into(), 2);

    let mut reactor = Reactor::new_for_test(LayoutEngine::new(
        &settings,
        &crate::common::config::LayoutSettings::default(),
        None,
    ));
    let screen1 = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let screen2 = CGRect::new(CGPoint::new(1000., 0.), CGSize::new(1000., 1000.));
    let space1 = SpaceId::new(1);
    let space2 = SpaceId::new(2);
    reactor.handle_event(screen_params_event(
        vec![screen1, screen2],
        vec![Some(space1), Some(space2)],
        vec![],
    ));

    let _ = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager_mut()
        .list_workspaces(space1);
    let _ = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager_mut()
        .list_workspaces(space2);

    let ws1 = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager()
        .active_workspace(space1)
        .expect("space1 should have ws1 active");
    let ws2 = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager()
        .active_workspace(space2)
        .expect("space2 should have ws2 active");
    let space2_uuid = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager()
        .space_display(space2)
        .expect("space2 has a display uuid")
        .to_owned();
    let ws3 = reactor.layout_manager.layout_engine.create_workspace_on_display(
        3,
        &space2_uuid,
        space2,
        screen2.size,
    );
    assert!(
        reactor
            .layout_manager
            .layout_engine
            .virtual_workspace_manager_mut()
            .set_active_workspace(space2, ws3),
        "precondition: ws3 should be active on the target display",
    );

    let mut apps = Apps::new();
    let first = WindowId::new(60, 1);
    let moved = WindowId::new(60, 2);
    reactor.handle_events(apps.make_app_with_opts(
        60,
        vec![make_window(1)],
        Some(first),
        true,
        true,
    ));
    reactor.handle_event(Event::ApplicationGloballyActivated(60));
    assert_eq!(
        reactor
            .layout_manager
            .layout_engine
            .virtual_workspace_manager()
            .workspace_for_window(first),
        Some(ws1),
    );

    reactor.handle_event(Event::ApplicationMainWindowChanged(
        60,
        Some(moved),
        Quiet::No,
    ));
    reactor.handle_event(Event::Command(Command::Layout(
        LayoutCommand::MoveWindowToWorkspace { workspace: 2, window_id: None },
    )));
    assert_eq!(
        reactor
            .layout_manager
            .layout_engine
            .virtual_workspace_manager()
            .workspace_for_window(moved),
        None,
        "precondition: move command raced before the new window entered VWM",
    );

    let mut target_frame = make_window(2);
    target_frame.frame.origin = CGPoint::new(screen2.origin.x + 200.0, screen2.origin.y + 200.0);
    reactor.handle_event(Event::WindowsDiscovered {
        pid: 60,
        new: vec![(moved, target_frame)],
        known_visible: vec![first, moved],
    });
    assert_eq!(
        reactor
            .layout_manager
            .layout_engine
            .virtual_workspace_manager()
            .workspace_for_window(moved),
        Some(ws2),
        "first discovery should honor the pending ws2 move target",
    );

    let mut stale_source_frame = make_window(2);
    stale_source_frame.frame.origin =
        CGPoint::new(screen1.origin.x + 200.0, screen1.origin.y + 200.0);
    reactor.handle_event(Event::WindowsDiscovered {
        pid: 60,
        new: vec![(moved, stale_source_frame)],
        known_visible: vec![first, moved],
    });

    assert_eq!(
        reactor
            .layout_manager
            .layout_engine
            .virtual_workspace_manager()
            .workspace_for_window(first),
        Some(ws1),
    );
    assert_eq!(
        reactor
            .layout_manager
            .layout_engine
            .virtual_workspace_manager()
            .workspace_for_window(moved),
        Some(ws2),
        "stale app refresh must not collect the pending-moved window back to ws1",
    );
}

// Regression: same-app refresh must not reclaim a manually moved window.
#[test]
fn moved_window_stays_on_cross_display_workspace_after_app_refresh() {
    let mut settings = crate::common::config::VirtualWorkspaceSettings::default();
    settings.display_default_workspaces.insert("test-display-0".into(), 1);
    settings.display_default_workspaces.insert("test-display-1".into(), 2);

    let mut reactor = Reactor::new_for_test(LayoutEngine::new(
        &settings,
        &crate::common::config::LayoutSettings::default(),
        None,
    ));
    let screen1 = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let screen2 = CGRect::new(CGPoint::new(1000., 0.), CGSize::new(1000., 1000.));
    let space1 = SpaceId::new(1);
    let space2 = SpaceId::new(2);
    reactor.handle_event(screen_params_event(
        vec![screen1, screen2],
        vec![Some(space1), Some(space2)],
        vec![],
    ));

    let _ = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager_mut()
        .list_workspaces(space1);
    let _ = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager_mut()
        .list_workspaces(space2);

    let ws1 = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager()
        .active_workspace(space1)
        .expect("space1 should have ws1 active");
    let ws2 = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager()
        .active_workspace(space2)
        .expect("space2 should have ws2 active");

    let mut apps = Apps::new();
    let first = WindowId::new(50, 1);
    let moved = WindowId::new(50, 2);
    reactor.handle_events(apps.make_app_with_opts(
        50,
        make_windows(2),
        Some(moved),
        true,
        true,
    ));
    reactor.handle_event(Event::ApplicationGloballyActivated(50));
    assert_eq!(
        reactor
            .layout_manager
            .layout_engine
            .virtual_workspace_manager()
            .workspace_for_window(first),
        Some(ws1),
        "precondition: first app window starts on ws1",
    );
    assert_eq!(
        reactor
            .layout_manager
            .layout_engine
            .virtual_workspace_manager()
            .workspace_for_window(moved),
        Some(ws1),
        "precondition: second app window starts on ws1",
    );

    reactor.send_layout_event(LayoutEvent::WindowFocused(space1, first));

    reactor.handle_event(Event::Command(Command::Layout(
        LayoutCommand::MoveWindowToWorkspace { workspace: 2, window_id: None },
    )));
    assert_eq!(
        reactor
            .layout_manager
            .layout_engine
            .virtual_workspace_manager()
            .workspace_for_window(moved),
        Some(ws2),
        "precondition: move command places the second app window on ws2",
    );

    if let Some(window) = reactor.window_manager.window_mut(moved) {
        window.frame_monotonic.origin =
            CGPoint::new(screen1.origin.x + 200.0, screen1.origin.y + 200.0);
    }

    let app_info = reactor.app_manager.apps.get(&moved.pid).unwrap().info.clone();
    reactor.process_windows_for_app_rules(moved.pid, vec![moved], app_info);

    assert_eq!(
        reactor
            .layout_manager
            .layout_engine
            .virtual_workspace_manager()
            .workspace_for_window(first),
        Some(ws1),
        "the original Chrome window should stay on ws1",
    );
    assert_eq!(
        reactor
            .layout_manager
            .layout_engine
            .virtual_workspace_manager()
            .workspace_for_window(moved),
        Some(ws2),
        "app refresh must not collect the moved Chrome window back onto ws1",
    );
}

#[test]
fn dragged_cross_display_window_resists_stale_app_refresh() {
    let mut settings = crate::common::config::VirtualWorkspaceSettings::default();
    settings.display_default_workspaces.insert("test-display-0".into(), 1);
    settings.display_default_workspaces.insert("test-display-1".into(), 2);

    let mut reactor = Reactor::new_for_test(LayoutEngine::new(
        &settings,
        &crate::common::config::LayoutSettings::default(),
        None,
    ));
    let screen1 = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let screen2 = CGRect::new(CGPoint::new(1000., 0.), CGSize::new(1000., 1000.));
    let space1 = SpaceId::new(1);
    let space2 = SpaceId::new(2);
    reactor.handle_event(screen_params_event(
        vec![screen1, screen2],
        vec![Some(space1), Some(space2)],
        vec![],
    ));

    let _ = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager_mut()
        .list_workspaces(space1);
    let _ = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager_mut()
        .list_workspaces(space2);

    let ws1 = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager()
        .active_workspace(space1)
        .expect("space1 should have ws1 active");
    let ws2 = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager()
        .active_workspace(space2)
        .expect("space2 should have ws2 active");

    let mut apps = Apps::new();
    let win = WindowId::new(70, 1);
    reactor.handle_events(apps.make_app_with_opts(
        70,
        vec![make_window(1)],
        Some(win),
        true,
        true,
    ));

    assert_eq!(
        reactor
            .layout_manager
            .layout_engine
            .virtual_workspace_manager()
            .workspace_for_window(win),
        Some(ws1),
        "precondition: window starts on ws1",
    );

    let source_frame = reactor.window_manager.window(win).unwrap().frame_monotonic;
    let mut target_frame = source_frame;
    target_frame.origin = CGPoint::new(screen2.origin.x + 200.0, screen2.origin.y + 200.0);

    reactor.handle_event(Event::WindowFrameChanged(
        win,
        target_frame,
        None,
        Requested(false),
        Some(MouseState::Down),
    ));
    reactor.handle_event(Event::MouseUp);

    assert_eq!(
        reactor
            .layout_manager
            .layout_engine
            .virtual_workspace_manager()
            .workspace_for_window(win),
        Some(ws2),
        "precondition: mouse-up drag assigns the window to space2's active workspace",
    );

    let mut stale_source_frame = make_window(1);
    stale_source_frame.frame = source_frame;
    reactor.handle_event(Event::WindowsDiscovered {
        pid: 70,
        new: vec![(win, stale_source_frame)],
        known_visible: vec![win],
    });

    assert_eq!(
        reactor
            .layout_manager
            .layout_engine
            .virtual_workspace_manager()
            .workspace_for_window(win),
        Some(ws2),
        "stale app refresh must not collect the dragged window back to ws1",
    );
}

// Hyprland semantics: when ws#N already exists on another display,
// MoveWindowToWorkspace { N } moves the window to that workspace's bound
// display. It must not create a duplicate ws#N on the source display, and
// focus must stay on the source display.
#[test]
fn move_window_to_workspace_moves_to_existing_workspace_on_other_display() {
    let TwoSpaceFixture {
        mut reactor,
        screen2,
        space1,
        space2,
        ..
    } = two_space_fixture();
    // Lazy-init both spaces' default workspaces so their display uuids land
    // in the VWM mirror (the move handler needs space1's uuid to create on).
    let _ = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager_mut()
        .list_workspaces(space1);
    let _ = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager_mut()
        .list_workspaces(space2);

    let space2_uuid = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager()
        .space_display(space2)
        .expect("space2 has a display uuid")
        .to_owned();

    // Pre-create ws 4 on space2 (the "other" display).
    let ws4_on_space2 = reactor.layout_manager.layout_engine.create_workspace_on_display(
        4,
        &space2_uuid,
        space2,
        screen2.size,
    );
    let pre = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager()
        .resolve_workspace(4)
        .expect("precondition: slot 4 must resolve after pre-creation");
    assert_eq!(
        pre.space, space2,
        "precondition: slot 4 currently lives on space2"
    );
    assert_eq!(
        pre.workspace_id, ws4_on_space2,
        "precondition: slot 4 resolves to the just-created ws on space2"
    );

    // Create a window on space1 (source display).
    let mut apps = Apps::new();
    reactor.handle_events(apps.make_app(50, make_windows(1)));
    let win = WindowId::new(50, 1);
    let source_ws = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager()
        .workspace_for_window(win)
        .expect("precondition: window must land on a workspace on space1");

    // Issue the move. ws 4 lives on space2 != source_space (space1), so the
    // window should cross displays onto the existing global workspace.
    reactor.handle_event(Event::Command(Command::Layout(
        LayoutCommand::MoveWindowToWorkspace {
            workspace: 4,
            window_id: Some(win.idx.get()),
        },
    )));

    let ws_for_win = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager()
        .workspace_for_window(win)
        .expect("window must still be tracked after the move");
    assert_ne!(
        ws_for_win, source_ws,
        "window must have left its source workspace"
    );
    assert_eq!(
        ws_for_win, ws4_on_space2,
        "window must move onto space2's pre-existing ws 4"
    );
    assert_eq!(
        reactor
            .layout_manager
            .layout_engine
            .virtual_workspace_manager()
            .workspace_space(ws_for_win),
        Some(space2),
        "the target workspace stays bound to space2"
    );

    // The pre-existing ws 4 on space2 must still exist as a workspace
    // (nothing destroyed it on the cross-space create-on-source-space path).
    assert_eq!(
        reactor
            .layout_manager
            .layout_engine
            .virtual_workspace_manager()
            .workspace_space(ws4_on_space2),
        Some(space2),
        "pre-existing ws 4 on space2 must still exist and still be on space2",
    );

    let post = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager()
        .resolve_workspace(4)
        .expect("slot 4 must still resolve");
    assert_eq!(
        post.space, space2,
        "slot 4 still resolves to its original display",
    );
    assert_eq!(
        post.workspace_id, ws4_on_space2,
        "slot 4 resolves to the pre-existing space2 ws",
    );

    assert_eq!(
        reactor.layout_manager.layout_engine.active_workspace(space1),
        Some(source_ws),
        "focus/active workspace must stay on the source display after cross-display move"
    );
}

// Task 3.4: When a display is unplugged, the windows on that display's
// workspaces must migrate to the active workspace of a remaining display,
// and the dead workspaces must be destroyed (freeing their numbers for
// re-use). The window must remain alive — just on a different space —
// and `resolve_workspace(N)` must no longer return the dead space for any
// surviving workspace number.
#[test]
fn display_unplug_migrates_windows_to_remaining_display() {
    let TwoSpaceFixture {
        mut reactor,
        screen1,
        screen2: _,
        space1,
        space2,
    } = two_space_fixture();

    // Lazy-init both spaces so each has a default workspace and the
    // display-uuid mirror is populated.
    let _ = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager_mut()
        .list_workspaces(space1);
    let _ = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager_mut()
        .list_workspaces(space2);

    // Spawn a window. `make_app` doesn't pick the space — that depends on
    // wherever the discovery handler routes the window — so we'll relocate
    // it to space2 explicitly via the VWM API below.
    let mut apps = Apps::new();
    reactor.handle_events(apps.make_app(60, make_windows(1)));
    let win = WindowId::new(60, 1);

    // Move the window onto space2's active workspace. We don't care which
    // space `make_app` picked; `assign_window_to_workspace` rewires
    // `window_to_workspace` and removes the window from any prior workspace.
    let active_ws_space2 = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager()
        .active_workspace(space2)
        .expect("space2 has an active workspace after lazy init");
    let (assigned, destroyed) = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager_mut()
        .assign_window_to_workspace(space2, win, active_ws_space2);
    assert!(
        assigned,
        "precondition: window must be assignable to space2's active ws"
    );
    for (sp, ws_id) in destroyed {
        reactor.layout_manager.layout_engine.drop_workspace_layout(sp, ws_id);
    }
    assert_eq!(
        reactor
            .layout_manager
            .layout_engine
            .virtual_workspace_manager()
            .workspace_for_window(win),
        Some(active_ws_space2),
        "precondition: window now lives on space2's active workspace"
    );

    // Unplug screen2: send a screen_params_event with only screen1.
    reactor.handle_event(screen_params_event(vec![screen1], vec![Some(space1)], vec![]));

    // The window must still be alive, now living on space1's active ws.
    let active_on_space1 = reactor
        .layout_manager
        .layout_engine
        .active_workspace(space1)
        .expect("space1 still has an active workspace after the unplug");
    let ws_for_win = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager()
        .workspace_for_window(win)
        .expect("window must have migrated to space1");
    assert_eq!(
        ws_for_win, active_on_space1,
        "window must land on space1's active workspace after the migration"
    );

    // No surviving workspace number should still resolve to space2 — the
    // dead workspaces have been destroyed and their numbers freed.
    let nums: Vec<_> = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager()
        .all_workspace_numbers()
        .collect();
    assert!(!nums.is_empty(), "space1's workspaces must still resolve");
    for n in &nums {
        let target = reactor
            .layout_manager
            .layout_engine
            .virtual_workspace_manager()
            .resolve_workspace(*n)
            .expect("every reported number must resolve");
        assert_ne!(
            target.space, space2,
            "no surviving workspace may live on the unplugged display's space"
        );
    }
}

#[test]
fn query_workspaces_by_display_uuid_matches_by_space_id() {
    // Task 5.1: rift-cli `query workspaces --display-uuid <uuid>` should
    // resolve the uuid to a SpaceId via the VWM and return the same
    // workspaces as `--space-id <space>` for the space bound to that uuid.
    let mut apps = Apps::new();
    let mut reactor = Reactor::new_for_test(LayoutEngine::new(
        &crate::common::config::VirtualWorkspaceSettings::default(),
        &crate::common::config::LayoutSettings::default(),
        None,
    ));
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let space = SpaceId::new(1);

    reactor.handle_event(screen_params_event(vec![screen], vec![Some(space)], vec![]));
    reactor.handle_events(apps.make_app(1, make_windows(2)));
    apps.simulate_until_quiet(&mut reactor);

    let display_uuid = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager()
        .space_display(space)
        .expect("space has a display uuid after screen-params handling")
        .to_owned();

    reactor.layout_manager.layout_engine.create_workspace_on_display(
        7,
        &display_uuid,
        space,
        screen.size,
    );

    let by_space = reactor.query_workspaces(Some(space), None);
    let by_uuid = reactor.query_workspaces(None, Some(display_uuid.clone()));

    assert!(!by_space.is_empty(), "expected at least one workspace for space");
    assert_eq!(
        by_space.len(),
        by_uuid.len(),
        "display-uuid query and space-id query must return same workspace count"
    );
    for (a, b) in by_space.iter().zip(by_uuid.iter()) {
        assert_eq!(a.id, b.id);
        assert_eq!(a.index, b.index);
        assert_eq!(a.number, b.number);
        assert_eq!(a.name, b.name);
        assert_eq!(a.is_active, b.is_active);
        assert_eq!(a.window_count, b.window_count);
    }
    assert!(
        by_uuid.iter().any(|w| w.number == 7 && w.index != w.number && w.name == "7"),
        "query output must expose the global workspace number separately from per-display index"
    );
}

#[test]
fn query_workspaces_by_display_uuid_stale_returns_empty() {
    // Task 5.1 spec compliance: an unresolvable display uuid must return an
    // empty result rather than silently falling back to the default query
    // space. This locks in the (None, Some(stale_uuid)) -> Vec::new() path
    // so future refactors can't reintroduce the silent fallback.
    let mut apps = Apps::new();
    let mut reactor = Reactor::new_for_test(LayoutEngine::new(
        &crate::common::config::VirtualWorkspaceSettings::default(),
        &crate::common::config::LayoutSettings::default(),
        None,
    ));
    let screen = CGRect::new(CGPoint::new(0., 0.), CGSize::new(1000., 1000.));
    let space = SpaceId::new(1);

    reactor.handle_event(screen_params_event(vec![screen], vec![Some(space)], vec![]));
    reactor.handle_events(apps.make_app(1, make_windows(2)));
    apps.simulate_until_quiet(&mut reactor);

    // Sanity: the default-space path still returns workspaces, so the empty
    // result below is genuinely due to the stale uuid and not an empty model.
    let by_default = reactor.query_workspaces(None, None);
    assert!(
        !by_default.is_empty(),
        "default-space query should return workspaces for the active reactor"
    );

    let stale = reactor.query_workspaces(None, Some("__nonexistent_uuid__".into()));
    assert!(
        stale.is_empty(),
        "unresolvable display uuid must return empty, not fall back to default space"
    );
}
