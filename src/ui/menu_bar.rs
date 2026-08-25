// many ideas for how this works were taken from https://github.com/xiamaz/YabaiIndicator
use std::cell::RefCell;
use std::collections::HashMap;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{ClassType, DefinedClass, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{
    NSColor, NSControlStateValueOff, NSControlStateValueOn, NSEventModifierFlags, NSFont,
    NSFontAttributeName, NSFontWeightRegular, NSFontWeightSemibold,
    NSForegroundColorAttributeName, NSGraphicsContext, NSMenu, NSMenuItem, NSImage,
    NSRunningApplication, NSStatusBar, NSStatusItem, NSVariableStatusItemLength, NSView,
};
use objc2_core_foundation::{
    CFAttributedString, CFDictionary, CFRetained, CFString, CGFloat, CGPoint, CGRect, CGSize,
};
use objc2_core_graphics::{CGBlendMode, CGContext};
use objc2_core_text::CTLine;
use objc2_foundation::{
    MainThreadMarker, NSAttributedStringKey, NSDictionary, NSMutableDictionary, NSObject, NSRect,
    NSSize, NSString,
};
use tokio::sync::mpsc::UnboundedSender;
use tracing::debug;

use crate::actor::reactor::{Command as ReactorTopCommand, ReactorCommand};
use crate::actor::wm_controller::{WmCmd, WmCommand};
use crate::common::config::{
    ActiveWorkspaceLabel, MenuBarDisplayMode, MenuBarSettings, WorkspaceDisplayStyle,
    WorkspaceSelector, workspace_number_to_global_slot,
};
use crate::core::ids::WorkspaceId;
use crate::model::layout::LayoutCommand;
use crate::model::server::{WindowData, WorkspaceData};
use crate::sys::hotkey::{Hotkey, KeyCode, Modifiers};
use crate::sys::screen::SpaceId;
use crate::ui::common::compute_window_layout_metrics;

const CELL_WIDTH: f64 = 20.0;
const CELL_HEIGHT: f64 = 17.0;
const CELL_SPACING: f64 = 3.0;
const CORNER_RADIUS: f64 = 5.0;
const BORDER_WIDTH: f64 = 1.0;
const CONTENT_INSET: f64 = 2.0;
const FONT_SIZE: f64 = 12.0;
const LABEL_HORIZONTAL_INSET: f64 = 4.0;
const APP_ICON_SIZE: f64 = 12.0;
const APP_ICON_SPACING: f64 = 3.0;
const DISPLAY_GROUP_SPACING: f64 = 16.0;
const DISPLAY_SEPARATOR_HEIGHT: f64 = 12.0;
const DISPLAY_SEPARATOR_WIDTH: f64 = 2.0;
const LABEL_ACTIVE_BACKGROUND_ALPHA: f64 = 0.10;
const LABEL_ACTIVE_INDICATOR_ALPHA: f64 = 0.85;
const LABEL_INACTIVE_ICON_ALPHA: f64 = 0.72;

#[derive(Debug, Clone, Copy)]
pub enum MenuAction {
    ToggleSpaceActivated,
    NextWorkspace,
    PrevWorkspace,
    SwitchToWorkspace(usize),
    OpenGitHub,
    OpenDocumentation,
    OpenMatrix,
    OpenConfig,
    ReloadConfig,
    QuitLift,
}

pub struct MenuIcon {
    status_item: Retained<NSStatusItem>,
    view: Retained<MenuIconView>,
    menu: Retained<NSMenu>,
    menu_handler: Retained<MenuActionHandler>,
    mtm: MainThreadMarker,
    prev_width: f64,
}

impl MenuIcon {
    pub fn new(mtm: MainThreadMarker, action_tx: UnboundedSender<MenuAction>) -> Self {
        let status_bar = NSStatusBar::systemStatusBar();
        let status_item = status_bar.statusItemWithLength(NSVariableStatusItemLength);
        let view = MenuIconView::new(mtm);
        let menu_handler = MenuActionHandler::new(mtm, action_tx);
        let menu = build_status_menu(
            mtm,
            &menu_handler,
            SpaceId::new(0),
            true,
            &[],
            &[],
            &MenuShortcuts::default(),
        );
        status_item.setMenu(Some(&menu));
        if let Some(btn) = status_item.button(mtm) {
            btn.addSubview(&*view);
            view.setFrameSize(NSSize::new(0.0, 0.0));
            status_item.setVisible(true);
        }

        Self {
            status_item,
            view,
            menu,
            menu_handler,
            mtm,
            prev_width: 0.0,
        }
    }

    pub fn update(
        &mut self,
        active_space: SpaceId,
        active_space_is_activated: bool,
        workspaces: &[WorkspaceData],
        display_starts: &[usize],
        _active_workspace: Option<WorkspaceId>,
        _windows: &[WindowData],
        settings: &MenuBarSettings,
        hotkeys: &[(Hotkey, WmCommand)],
    ) {
        let shortcuts = MenuShortcuts::from_hotkeys(hotkeys);
        let menu = build_status_menu(
            self.mtm,
            &self.menu_handler,
            active_space,
            active_space_is_activated,
            workspaces,
            display_starts,
            &shortcuts,
        );
        self.status_item.setMenu(Some(&menu));
        self.menu = menu;

        let mode = settings.mode;
        let style = settings.display_style;
        let label_for = |workspace: &WorkspaceData| match settings.active_label {
            ActiveWorkspaceLabel::Index => workspace.number.to_string(),
            ActiveWorkspaceLabel::Name => {
                if workspace.name.is_empty() {
                    workspace.number.to_string()
                } else {
                    workspace.name.clone()
                }
            }
        };
        let mut display_group = 0;
        let grouped_workspaces = workspaces
            .iter()
            .cloned()
            .enumerate()
            .map(|(index, workspace)| {
                if display_starts.binary_search(&index).is_ok() {
                    display_group += 1;
                }
                (display_group, workspace)
            })
            .collect::<Vec<_>>();

        let render_inputs: Vec<WorkspaceRenderInput> = match (mode, style) {
            (MenuBarDisplayMode::All, WorkspaceDisplayStyle::Layout) => grouped_workspaces
                .into_iter()
                .filter(|(_, ws)| settings.show_empty || ws.window_count > 0 || ws.is_active)
                .map(|(display_group, ws)| WorkspaceRenderInput {
                        workspace: ws,
                        label: String::new(),
                        show_windows: true,
                        app_icon: None,
                        display_group,
                    })
                .collect(),
            (MenuBarDisplayMode::All, WorkspaceDisplayStyle::Label) => grouped_workspaces
                .into_iter()
                .filter(|(_, ws)| settings.show_empty || ws.window_count > 0 || ws.is_active)
                .map(|(display_group, ws)| {
                    let app_icon = primary_app_icon(&ws);
                    let mut clone = ws.clone();
                    clone.windows.clear();
                    WorkspaceRenderInput {
                        workspace: clone,
                        label: label_for(&ws),
                        show_windows: false,
                        app_icon,
                        display_group,
                    }
                })
                .collect(),
            (MenuBarDisplayMode::Active, WorkspaceDisplayStyle::Layout) => grouped_workspaces
                .into_iter()
                .filter(|(_, ws)| ws.is_active)
                .map(|(display_group, ws)| WorkspaceRenderInput {
                        workspace: ws,
                        label: String::new(),
                        show_windows: true,
                        app_icon: None,
                        display_group,
                })
                .collect(),
            (MenuBarDisplayMode::Active, WorkspaceDisplayStyle::Label) => grouped_workspaces
                .into_iter()
                .filter(|(_, ws)| ws.is_active)
                .map(|(display_group, ws)| {
                    let app_icon = primary_app_icon(&ws);
                    let mut clone = ws.clone();
                    clone.windows.clear();
                    WorkspaceRenderInput {
                        workspace: clone,
                        label: label_for(&ws),
                        show_windows: false,
                        app_icon,
                        display_group,
                    }
                })
                .collect(),
        };

        if render_inputs.is_empty() {
            self.status_item.setVisible(false);
            self.prev_width = 0.0;
            return;
        }

        let layout = {
            let view_ivars = self.view.ivars();
            let active_attrs = view_ivars.active_text_attrs.as_ref();
            let inactive_attrs = view_ivars.inactive_text_attrs.as_ref();
            let empty_attrs = view_ivars.empty_text_attrs.as_ref();
            build_layout(&render_inputs, active_attrs, inactive_attrs, empty_attrs)
        };
        if layout.workspaces.is_empty() {
            self.status_item.setVisible(false);
            self.prev_width = 0.0;
            return;
        }

        let size = NSSize::new(layout.total_width, layout.total_height);
        self.view.set_layout(layout);

        self.status_item.setLength(size.width);
        self.status_item.setVisible(true);

        if let Some(btn) = self.status_item.button(self.mtm) {
            if self.prev_width != size.width {
                self.prev_width = size.width;
                btn.setNeedsLayout(true);
            }

            self.view.setFrameSize(size);
            let btn_bounds = btn.bounds();
            let x = (btn_bounds.size.width - size.width) / 2.0;
            let y = (btn_bounds.size.height - size.height) / 2.0;
            self.view.setFrameOrigin(CGPoint::new(x, y));
        }

        self.view.setNeedsDisplay(true);
    }
}

impl Drop for MenuIcon {
    fn drop(&mut self) {
        debug!("Removing menu bar icon");

        let status_bar = NSStatusBar::systemStatusBar();
        status_bar.removeStatusItem(&self.status_item);
    }
}

#[derive(Default)]
struct MenuIconLayout {
    total_width: f64,
    total_height: f64,
    workspaces: Vec<WorkspaceRenderData>,
    separators: Vec<f64>,
}

struct WorkspaceRenderData {
    bg_rect: CGRect,
    fill_alpha: f64,
    is_active: bool,
    windows: Vec<CGRect>,
    label_line: Option<CachedTextLine>,
    label_x: f64,
    app_icon: Option<Retained<NSImage>>,
    app_icon_rect: Option<CGRect>,
    show_windows: bool,
}

struct WorkspaceRenderInput {
    workspace: WorkspaceData,
    label: String,
    show_windows: bool,
    app_icon: Option<Retained<NSImage>>,
    display_group: usize,
}

fn primary_app_icon(workspace: &WorkspaceData) -> Option<Retained<NSImage>> {
    let window = workspace.windows.first()?;
    NSRunningApplication::runningApplicationWithProcessIdentifier(window.id.pid)?.icon()
}

struct CachedTextLine {
    line: CFRetained<CTLine>,
    width: f64,
    ascent: f64,
    descent: f64,
}

struct MenuIconViewIvars {
    layout: RefCell<MenuIconLayout>,
    active_text_attrs: Retained<NSDictionary<NSAttributedStringKey, AnyObject>>,
    inactive_text_attrs: Retained<NSDictionary<NSAttributedStringKey, AnyObject>>,
    empty_text_attrs: Retained<NSDictionary<NSAttributedStringKey, AnyObject>>,
}

fn as_any_object<T: Message>(obj: &T) -> &AnyObject {
    unsafe { &*(obj as *const T as *const AnyObject) }
}

fn make_menu_item(
    mtm: MainThreadMarker,
    title: &str,
    action: Option<objc2::runtime::Sel>,
    target: Option<&MenuActionHandler>,
    checked: Option<bool>,
    key_equivalent: Option<&Hotkey>,
    tag: Option<isize>,
) -> Retained<NSMenuItem> {
    let ns_title = NSString::from_str(title);
    let key_equivalent_empty = NSString::from_str("");
    let item: Retained<NSMenuItem> = unsafe {
        msg_send![NSMenuItem::alloc(mtm), initWithTitle: &*ns_title, action: action, keyEquivalent: &*key_equivalent_empty]
    };
    if let Some(target) = target {
        unsafe {
            item.setTarget(Some(target));
        }
    }
    if let Some(checked) = checked {
        item.setState(if checked {
            NSControlStateValueOn
        } else {
            NSControlStateValueOff
        });
    }

    if let Some((key, modifiers)) = key_equivalent.and_then(menu_hotkey_to_key_equivalent) {
        let key = NSString::from_str(key);
        item.setKeyEquivalent(&key);
        item.setKeyEquivalentModifierMask(modifiers);
    }
    if let Some(tag) = tag {
        item.setTag(tag);
    }

    item
}

fn add_separator(menu: &NSMenu) {
    let separator: Retained<NSMenuItem> = unsafe { msg_send![NSMenuItem::class(), separatorItem] };
    menu.addItem(&separator);
}

fn build_status_menu(
    mtm: MainThreadMarker,
    handler: &MenuActionHandler,
    _active_space: SpaceId,
    active_space_is_activated: bool,
    workspaces: &[WorkspaceData],
    display_starts: &[usize],
    shortcuts: &MenuShortcuts,
) -> Retained<NSMenu> {
    let title = NSString::from_str("Lift");
    let menu: Retained<NSMenu> = unsafe { msg_send![NSMenu::alloc(mtm), initWithTitle: &*title] };

    let workspace_item = make_menu_item(mtm, "Workspaces", None, None, None, None, None);
    let ws_submenu_title = NSString::from_str("Workspace");
    let ws_submenu: Retained<NSMenu> =
        unsafe { msg_send![NSMenu::alloc(mtm), initWithTitle: &*ws_submenu_title] };

    ws_submenu.addItem(&make_menu_item(
        mtm,
        "Next Workspace",
        Some(sel!(onNextWorkspace:)),
        Some(handler),
        None,
        shortcuts.next_workspace.as_ref(),
        None,
    ));
    ws_submenu.addItem(&make_menu_item(
        mtm,
        "Previous Workspace",
        Some(sel!(onPrevWorkspace:)),
        Some(handler),
        None,
        shortcuts.prev_workspace.as_ref(),
        None,
    ));
    add_separator(&ws_submenu);

    for (workspace_index, ws) in workspaces.iter().enumerate() {
        if display_starts.binary_search(&workspace_index).is_ok() {
            add_separator(&ws_submenu);
        }
        let ws_label = if ws.name.is_empty() {
            format!("Workspace {}", ws.number)
        } else {
            format!("{} ({})", ws.name, ws.number)
        };
        let global_slot = workspace_number_to_global_slot(ws.number);
        let ws_shortcut = shortcuts
            .switch_workspace_by_index
            .get(&global_slot.unwrap_or(ws.index))
            .or_else(|| shortcuts.switch_workspace_by_name.get(&ws.name));
        let ws_item = make_menu_item(
            mtm,
            &ws_label,
            Some(sel!(onSwitchWorkspace:)),
            Some(handler),
            Some(ws.is_active),
            ws_shortcut,
            workspace_number_to_global_slot(ws.number).map(|slot| slot as isize),
        );
        ws_submenu.addItem(&ws_item);
    }
    if workspaces.is_empty() {
        workspace_item.setEnabled(false);
    } else {
        workspace_item.setSubmenu(Some(&ws_submenu));
    }
    menu.addItem(&workspace_item);

    menu.addItem(&make_menu_item(
        mtm,
        "Enable Tiling",
        Some(sel!(onToggleSpaceActivation:)),
        Some(handler),
        Some(active_space_is_activated),
        shortcuts.toggle_space_activation.as_ref(),
        None,
    ));

    add_separator(&menu);
    menu.addItem(&make_menu_item(
        mtm,
        "Settings…",
        Some(sel!(onOpenConfig:)),
        Some(handler),
        None,
        None,
        None,
    ));
    menu.addItem(&make_menu_item(
        mtm,
        "Reload Config",
        Some(sel!(onReloadConfig:)),
        Some(handler),
        None,
        None,
        None,
    ));

    let help_item = make_menu_item(mtm, "Help / Documentation", None, None, None, None, None);
    let help_submenu_title = NSString::from_str("Help / Documentation");
    let help_submenu: Retained<NSMenu> =
        unsafe { msg_send![NSMenu::alloc(mtm), initWithTitle: &*help_submenu_title] };
    help_submenu.addItem(&make_menu_item(
        mtm,
        "Documentation",
        Some(sel!(onOpenDocumentation:)),
        Some(handler),
        None,
        None,
        None,
    ));
    help_submenu.addItem(&make_menu_item(
        mtm,
        "GitHub",
        Some(sel!(onOpenGitHub:)),
        Some(handler),
        None,
        None,
        None,
    ));
    help_submenu.addItem(&make_menu_item(
        mtm,
        "Matrix",
        Some(sel!(onOpenMatrix:)),
        Some(handler),
        None,
        None,
        None,
    ));
    help_item.setSubmenu(Some(&help_submenu));
    menu.addItem(&help_item);

    add_separator(&menu);
    menu.addItem(&make_menu_item(
        mtm,
        "Quit Lift",
        Some(sel!(onQuitLift:)),
        Some(handler),
        None,
        shortcuts.quit_lift.as_ref(),
        None,
    ));

    menu
}

#[derive(Default)]
struct MenuShortcuts {
    toggle_space_activation: Option<Hotkey>,
    next_workspace: Option<Hotkey>,
    prev_workspace: Option<Hotkey>,
    quit_lift: Option<Hotkey>,
    switch_workspace_by_index: HashMap<usize, Hotkey>,
    switch_workspace_by_name: HashMap<String, Hotkey>,
}

impl MenuShortcuts {
    fn from_hotkeys(hotkeys: &[(Hotkey, WmCommand)]) -> Self {
        let mut out = Self::default();

        for (hotkey, command) in hotkeys {
            match command {
                WmCommand::Wm(WmCmd::ToggleSpaceActivated) => {
                    out.toggle_space_activation.get_or_insert_with(|| hotkey.clone());
                }
                WmCommand::Wm(WmCmd::NextWorkspace) => {
                    out.next_workspace.get_or_insert_with(|| hotkey.clone());
                }
                WmCommand::Wm(WmCmd::PrevWorkspace) => {
                    out.prev_workspace.get_or_insert_with(|| hotkey.clone());
                }
                WmCommand::Wm(WmCmd::SwitchToWorkspace(WorkspaceSelector::Index(i))) => {
                    if let Some(slot) = workspace_number_to_global_slot(*i) {
                        out.switch_workspace_by_index.entry(slot).or_insert_with(|| hotkey.clone());
                    }
                }
                WmCommand::Wm(WmCmd::SwitchToWorkspace(WorkspaceSelector::Name(name))) => {
                    out.switch_workspace_by_name
                        .entry(name.clone())
                        .or_insert_with(|| hotkey.clone());
                }
                WmCommand::ReactorCommand(ReactorTopCommand::Reactor(
                    ReactorCommand::ToggleSpaceActivated,
                )) => {
                    out.toggle_space_activation.get_or_insert_with(|| hotkey.clone());
                }
                WmCommand::ReactorCommand(ReactorTopCommand::Layout(
                    LayoutCommand::NextWorkspace(_),
                )) => {
                    out.next_workspace.get_or_insert_with(|| hotkey.clone());
                }
                WmCommand::ReactorCommand(ReactorTopCommand::Layout(
                    LayoutCommand::PrevWorkspace(_),
                )) => {
                    out.prev_workspace.get_or_insert_with(|| hotkey.clone());
                }
                WmCommand::ReactorCommand(ReactorTopCommand::Layout(
                    LayoutCommand::SwitchToWorkspace(i),
                )) => {
                    out.switch_workspace_by_index.entry(*i).or_insert_with(|| hotkey.clone());
                }
                WmCommand::ReactorCommand(ReactorTopCommand::Reactor(
                    ReactorCommand::SaveAndExit,
                )) => {
                    out.quit_lift.get_or_insert_with(|| hotkey.clone());
                }
                _ => {}
            }
        }

        out
    }
}

fn menu_hotkey_to_key_equivalent(hotkey: &Hotkey) -> Option<(&'static str, NSEventModifierFlags)> {
    let key = match hotkey.key_code {
        KeyCode::KeyA => "a",
        KeyCode::KeyB => "b",
        KeyCode::KeyC => "c",
        KeyCode::KeyD => "d",
        KeyCode::KeyE => "e",
        KeyCode::KeyF => "f",
        KeyCode::KeyG => "g",
        KeyCode::KeyH => "h",
        KeyCode::KeyI => "i",
        KeyCode::KeyJ => "j",
        KeyCode::KeyK => "k",
        KeyCode::KeyL => "l",
        KeyCode::KeyM => "m",
        KeyCode::KeyN => "n",
        KeyCode::KeyO => "o",
        KeyCode::KeyP => "p",
        KeyCode::KeyQ => "q",
        KeyCode::KeyR => "r",
        KeyCode::KeyS => "s",
        KeyCode::KeyT => "t",
        KeyCode::KeyU => "u",
        KeyCode::KeyV => "v",
        KeyCode::KeyW => "w",
        KeyCode::KeyX => "x",
        KeyCode::KeyY => "y",
        KeyCode::KeyZ => "z",
        KeyCode::Digit0 => "0",
        KeyCode::Digit1 => "1",
        KeyCode::Digit2 => "2",
        KeyCode::Digit3 => "3",
        KeyCode::Digit4 => "4",
        KeyCode::Digit5 => "5",
        KeyCode::Digit6 => "6",
        KeyCode::Digit7 => "7",
        KeyCode::Digit8 => "8",
        KeyCode::Digit9 => "9",
        KeyCode::Minus => "-",
        KeyCode::Equal => "=",
        KeyCode::BracketLeft => "[",
        KeyCode::BracketRight => "]",
        KeyCode::Semicolon => ";",
        KeyCode::Quote => "'",
        KeyCode::Backquote => "`",
        KeyCode::Backslash => "\\",
        KeyCode::Comma => ",",
        KeyCode::Period => ".",
        KeyCode::Slash => "/",
        _ => return None,
    };

    let mut flags = NSEventModifierFlags::empty();
    if hotkey.modifiers.intersects(Modifiers::META) {
        flags.insert(NSEventModifierFlags::Command);
    }
    if hotkey.modifiers.intersects(Modifiers::CONTROL) {
        flags.insert(NSEventModifierFlags::Control);
    }
    if hotkey.modifiers.intersects(Modifiers::ALT) {
        flags.insert(NSEventModifierFlags::Option);
    }
    if hotkey.modifiers.intersects(Modifiers::SHIFT) {
        flags.insert(NSEventModifierFlags::Shift);
    }

    Some((key, flags))
}

struct MenuActionHandlerIvars {
    action_tx: UnboundedSender<MenuAction>,
}

impl MenuActionHandler {
    fn new(mtm: MainThreadMarker, action_tx: UnboundedSender<MenuAction>) -> Retained<Self> {
        let this = mtm.alloc().set_ivars(MenuActionHandlerIvars { action_tx });
        unsafe { msg_send![super(this), init] }
    }

    fn emit(&self, action: MenuAction) {
        let _ = self.ivars().action_tx.send(action);
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "LiftMenuBarActionHandler"]
    #[ivars = MenuActionHandlerIvars]
    struct MenuActionHandler;

    impl MenuActionHandler {
        #[unsafe(method(onToggleSpaceActivation:))]
        fn on_toggle_space_activation(&self, _sender: Option<&AnyObject>) {
            self.emit(MenuAction::ToggleSpaceActivated);
        }

        #[unsafe(method(onNextWorkspace:))]
        fn on_next_workspace(&self, _sender: Option<&AnyObject>) {
            self.emit(MenuAction::NextWorkspace);
        }

        #[unsafe(method(onPrevWorkspace:))]
        fn on_prev_workspace(&self, _sender: Option<&AnyObject>) {
            self.emit(MenuAction::PrevWorkspace);
        }

        #[unsafe(method(onSwitchWorkspace:))]
        fn on_switch_workspace(&self, sender: Option<&NSMenuItem>) {
            if let Some(sender) = sender {
                let tag = sender.tag();
                if tag >= 0 {
                    self.emit(MenuAction::SwitchToWorkspace(tag as usize));
                }
            }
        }

        #[unsafe(method(onOpenConfig:))]
        fn on_open_config(&self, _sender: Option<&AnyObject>) {
            self.emit(MenuAction::OpenConfig);
        }

        #[unsafe(method(onOpenDocumentation:))]
        fn on_open_documentation(&self, _sender: Option<&AnyObject>) {
            self.emit(MenuAction::OpenDocumentation);
        }

        #[unsafe(method(onOpenGitHub:))]
        fn on_open_github(&self, _sender: Option<&AnyObject>) {
            self.emit(MenuAction::OpenGitHub);
        }

        #[unsafe(method(onOpenMatrix:))]
        fn on_open_matrix(&self, _sender: Option<&AnyObject>) {
            self.emit(MenuAction::OpenMatrix);
        }

        #[unsafe(method(onReloadConfig:))]
        fn on_reload_config(&self, _sender: Option<&AnyObject>) {
            self.emit(MenuAction::ReloadConfig);
        }

        #[unsafe(method(onQuitLift:))]
        fn on_quit_lift(&self, _sender: Option<&AnyObject>) {
            self.emit(MenuAction::QuitLift);
        }
    }
);

fn build_text_attrs(
    font: &NSFont,
    color: &NSColor,
) -> Retained<NSDictionary<NSAttributedStringKey, AnyObject>> {
    let dict = NSMutableDictionary::<NSAttributedStringKey, AnyObject>::new();
    unsafe {
        dict.setObject_forKeyedSubscript(
            Some(as_any_object(font)),
            ProtocolObject::from_ref(NSFontAttributeName),
        );
        dict.setObject_forKeyedSubscript(
            Some(as_any_object(color)),
            ProtocolObject::from_ref(NSForegroundColorAttributeName),
        );
    }
    unsafe { Retained::cast_unchecked(dict) }
}

fn build_cached_text_line(
    label: &str,
    attrs: &NSDictionary<NSAttributedStringKey, AnyObject>,
) -> Option<CachedTextLine> {
    if label.is_empty() {
        return None;
    }

    let label_ns = NSString::from_str(label);
    let cf_string: &CFString = label_ns.as_ref();
    let cf_dict_ref: &CFDictionary<NSAttributedStringKey, AnyObject> = attrs.as_ref();
    let cf_dict: &CFDictionary = cf_dict_ref.as_opaque();
    let attr_string = unsafe { CFAttributedString::new(None, Some(cf_string), Some(cf_dict)) }?;
    let line: CFRetained<CTLine> = unsafe { CTLine::with_attributed_string(attr_string.as_ref()) };

    let mut ascent: CGFloat = 0.0;
    let mut descent: CGFloat = 0.0;
    let mut leading: CGFloat = 0.0;
    let line_ref: &CTLine = line.as_ref();
    let width = unsafe { line_ref.typographic_bounds(&mut ascent, &mut descent, &mut leading) };

    Some(CachedTextLine {
        line,
        width: width as f64,
        ascent: ascent as f64,
        descent: descent as f64,
    })
}

impl MenuIconView {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let active_font = NSFont::monospacedDigitSystemFontOfSize_weight(
            FONT_SIZE,
            unsafe { NSFontWeightSemibold },
        );
        let inactive_font = NSFont::monospacedDigitSystemFontOfSize_weight(
            FONT_SIZE,
            unsafe { NSFontWeightRegular },
        );
        let active_color = NSColor::labelColor();
        let inactive_color = NSColor::secondaryLabelColor();
        let empty_color = NSColor::tertiaryLabelColor();
        let active_attrs = build_text_attrs(active_font.as_ref(), active_color.as_ref());
        let inactive_attrs = build_text_attrs(inactive_font.as_ref(), inactive_color.as_ref());
        let empty_attrs = build_text_attrs(inactive_font.as_ref(), empty_color.as_ref());

        let frame = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(0.0, 0.0));
        let view = mtm.alloc().set_ivars(MenuIconViewIvars {
            layout: RefCell::new(MenuIconLayout::default()),
            active_text_attrs: active_attrs,
            inactive_text_attrs: inactive_attrs,
            empty_text_attrs: empty_attrs,
        });
        unsafe { msg_send![super(view), initWithFrame: frame] }
    }

    fn set_layout(&self, layout: MenuIconLayout) {
        *self.ivars().layout.borrow_mut() = layout;
        self.setNeedsDisplay(true);
    }
}

fn build_layout(
    inputs: &[WorkspaceRenderInput],
    active_attrs: &NSDictionary<NSAttributedStringKey, AnyObject>,
    inactive_attrs: &NSDictionary<NSAttributedStringKey, AnyObject>,
    empty_attrs: &NSDictionary<NSAttributedStringKey, AnyObject>,
) -> MenuIconLayout {
    let total_height = CELL_HEIGHT;

    let mut workspaces = Vec::with_capacity(inputs.len());
    let mut separators = Vec::new();
    let mut next_x = 0.0;
    for (i, input) in inputs.iter().enumerate() {
        let workspace = &input.workspace;
        let fill_alpha = if input.show_windows {
            if workspace.is_active {
                1.0
            } else if workspace.window_count > 0 {
                0.45
            } else {
                0.0
            }
        } else {
            label_fill_alpha(workspace.is_active)
        };

        let label_line = if !input.label.is_empty() {
            let attrs = if workspace.is_active {
                active_attrs
            } else if workspace.window_count == 0 {
                empty_attrs
            } else {
                inactive_attrs
            };
            build_cached_text_line(&input.label, attrs)
        } else {
            None
        };

        if i > 0 {
            let display_boundary = input.display_group != inputs[i - 1].display_group;
            if display_boundary {
                separators.push(next_x + DISPLAY_GROUP_SPACING / 2.0);
            }
            next_x += inter_workspace_spacing(display_boundary);
        }
        let label_width = label_line.as_ref().map_or(0.0, |line| line.width);
        let has_icon = input.app_icon.is_some();
        let content_width = label_width
            + if has_icon {
                APP_ICON_SIZE + APP_ICON_SPACING
            } else {
                0.0
            };
        let cell_width = label_cell_width(label_width, has_icon);
        let bg_rect = CGRect::new(
            CGPoint::new(next_x, 0.0),
            CGSize::new(cell_width, CELL_HEIGHT),
        );
        let content_x = next_x + (cell_width - content_width) / 2.0;
        let (label_x, app_icon_x) = label_and_icon_positions(content_x, label_width, has_icon);
        let app_icon_rect = app_icon_x.map(|icon_x| {
            CGRect::new(
                CGPoint::new(icon_x, (CELL_HEIGHT - APP_ICON_SIZE) / 2.0),
                CGSize::new(APP_ICON_SIZE, APP_ICON_SIZE),
            )
        });

        let windows = if input.show_windows && !workspace.windows.is_empty() {
            let layout = compute_window_layout_metrics(
                &workspace.windows,
                bg_rect,
                CONTENT_INSET,
                1.0,
                None,
            );
            if let Some(layout) = layout {
                const MIN_TILE_SIZE: f64 = 2.0;
                const WIN_GAP: f64 = 0.75;
                let mut rects = Vec::with_capacity(workspace.windows.len());
                for window in workspace.windows.iter().rev() {
                    let rect = layout.rect_for(window, MIN_TILE_SIZE, WIN_GAP);
                    rects.push(rect);
                }
                rects
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };
        next_x += cell_width;

        workspaces.push(WorkspaceRenderData {
            bg_rect,
            fill_alpha,
            is_active: workspace.is_active,
            windows,
            label_line,
            label_x,
            app_icon: input.app_icon.clone(),
            app_icon_rect,
            show_windows: input.show_windows,
        });
    }

    MenuIconLayout {
        total_width: next_x,
        total_height,
        workspaces,
        separators,
    }
}

fn label_cell_width(label_width: f64, has_icon: bool) -> f64 {
    let icon_width = if has_icon {
        APP_ICON_SIZE + APP_ICON_SPACING
    } else {
        0.0
    };
    CELL_WIDTH.max(label_width + icon_width + LABEL_HORIZONTAL_INSET * 2.0)
}

fn label_fill_alpha(is_active: bool) -> f64 {
    if is_active { LABEL_ACTIVE_BACKGROUND_ALPHA } else { 0.0 }
}

fn label_and_icon_positions(
    content_x: f64,
    label_width: f64,
    has_icon: bool,
) -> (f64, Option<f64>) {
    let app_icon_x = has_icon.then_some(content_x + label_width + APP_ICON_SPACING);
    (content_x, app_icon_x)
}

fn inter_workspace_spacing(display_boundary: bool) -> f64 {
    if display_boundary { DISPLAY_GROUP_SPACING } else { CELL_SPACING }
}

fn add_rounded_rect(ctx: &CGContext, x: f64, y: f64, w: f64, h: f64, r: f64) {
    let ctx = Some(ctx);
    let r = r.min(w / 2.0).min(h / 2.0);
    CGContext::begin_path(ctx);
    CGContext::move_to_point(ctx, x + r, y + h);
    CGContext::add_line_to_point(ctx, x + w - r, y + h);
    CGContext::add_arc_to_point(ctx, x + w, y + h, x + w, y + h - r, r);
    CGContext::add_line_to_point(ctx, x + w, y + r);
    CGContext::add_arc_to_point(ctx, x + w, y, x + w - r, y, r);
    CGContext::add_line_to_point(ctx, x + r, y);
    CGContext::add_arc_to_point(ctx, x, y, x, y + r, r);
    CGContext::add_line_to_point(ctx, x, y + h - r);
    CGContext::add_arc_to_point(ctx, x, y + h, x + r, y + h, r);
    CGContext::close_path(ctx);
}

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "LiftMenuBarIconView"]
    #[ivars = MenuIconViewIvars]
    struct MenuIconView;

    impl MenuIconView {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty_rect: NSRect) {
            let layout = self.ivars().layout.borrow();
            let bounds = self.bounds();

            if let Some(context) = NSGraphicsContext::currentContext() {
                let cg_context = context.CGContext();
                let cg = cg_context.as_ref();
                CGContext::save_g_state(Some(cg));
                CGContext::clear_rect(Some(cg), bounds);

                let y_offset = (bounds.size.height - layout.total_height) / 2.0;
                let separator_color = NSColor::secondaryLabelColor().CGColor();
                let layout_border = NSColor::separatorColor().CGColor();
                let label_background = NSColor::labelColor().CGColor();
                let active_background = NSColor::controlAccentColor().CGColor();
                let inactive_background = NSColor::labelColor().CGColor();

                CGContext::set_fill_color_with_color(Some(cg), Some(separator_color.as_ref()));
                CGContext::set_alpha(Some(cg), 0.48);
                for separator_x in &layout.separators {
                    add_rounded_rect(
                        cg,
                        *separator_x - DISPLAY_SEPARATOR_WIDTH / 2.0,
                        y_offset + (CELL_HEIGHT - DISPLAY_SEPARATOR_HEIGHT) / 2.0,
                        DISPLAY_SEPARATOR_WIDTH,
                        DISPLAY_SEPARATOR_HEIGHT,
                        DISPLAY_SEPARATOR_WIDTH / 2.0,
                    );
                    CGContext::fill_path(Some(cg));
                }
                CGContext::set_alpha(Some(cg), 1.0);

                for workspace in layout.workspaces.iter() {
                    let rect = workspace.bg_rect;
                    let bg_y = rect.origin.y + y_offset;
                    if workspace.show_windows {
                        add_rounded_rect(
                            cg,
                            rect.origin.x,
                            bg_y,
                            rect.size.width,
                            rect.size.height,
                            CORNER_RADIUS,
                        );
                        CGContext::save_g_state(Some(cg));
                        let background = if workspace.is_active {
                            active_background.as_ref()
                        } else {
                            inactive_background.as_ref()
                        };
                        CGContext::set_fill_color_with_color(Some(cg), Some(background));
                        CGContext::set_alpha(
                            Some(cg),
                            workspace.fill_alpha,
                        );
                        CGContext::fill_path(Some(cg));
                        CGContext::restore_g_state(Some(cg));

                        add_rounded_rect(
                            cg,
                            rect.origin.x,
                            bg_y,
                            rect.size.width,
                            rect.size.height,
                            CORNER_RADIUS,
                        );
                        CGContext::set_stroke_color_with_color(
                            Some(cg),
                            Some(layout_border.as_ref()),
                        );
                        CGContext::set_line_width(Some(cg), BORDER_WIDTH);
                        CGContext::stroke_path(Some(cg));

                        for window in workspace.windows.iter() {
                            add_rounded_rect(
                                cg,
                                window.origin.x,
                                window.origin.y + y_offset,
                                window.size.width,
                                window.size.height,
                                1.5,
                            );
                            CGContext::set_rgb_fill_color(Some(cg), 1.0, 1.0, 1.0, 1.0);
                            CGContext::fill_path(Some(cg));

                            CGContext::save_g_state(Some(cg));
                            CGContext::set_blend_mode(Some(cg), CGBlendMode::DestinationOut);
                            CGContext::set_rgb_stroke_color(Some(cg), 1.0, 1.0, 1.0, 1.0);
                            CGContext::set_line_width(Some(cg), 1.5);
                            add_rounded_rect(
                                cg,
                                window.origin.x,
                                window.origin.y,
                                window.size.width,
                                window.size.height,
                                1.5,
                            );
                            CGContext::stroke_path(Some(cg));
                            CGContext::restore_g_state(Some(cg));
                        }
                    } else if workspace.is_active {
                        add_rounded_rect(
                            cg,
                            rect.origin.x,
                            bg_y,
                            rect.size.width,
                            rect.size.height,
                            CORNER_RADIUS,
                        );
                        CGContext::set_fill_color_with_color(
                            Some(cg),
                            Some(label_background.as_ref()),
                        );
                        CGContext::set_alpha(Some(cg), workspace.fill_alpha);
                        CGContext::fill_path(Some(cg));
                        CGContext::set_alpha(Some(cg), LABEL_ACTIVE_INDICATOR_ALPHA);
                        CGContext::set_fill_color_with_color(
                            Some(cg),
                            Some(active_background.as_ref()),
                        );
                        let indicator_width = (rect.size.width - 10.0).max(8.0);
                        add_rounded_rect(
                            cg,
                            rect.origin.x + (rect.size.width - indicator_width) / 2.0,
                            bg_y + 0.5,
                            indicator_width,
                            1.5,
                            0.75,
                        );
                        CGContext::fill_path(Some(cg));
                        CGContext::set_alpha(Some(cg), 1.0);
                    }

                    if let (Some(app_icon), Some(mut icon_rect)) =
                        (&workspace.app_icon, workspace.app_icon_rect)
                    {
                        icon_rect.origin.y += y_offset;
                        CGContext::save_g_state(Some(cg));
                        if !workspace.is_active {
                            CGContext::set_alpha(Some(cg), LABEL_INACTIVE_ICON_ALPHA);
                        }
                        app_icon.drawInRect(icon_rect);
                        CGContext::restore_g_state(Some(cg));
                    }

                    if let Some(label_line) = &workspace.label_line {
                        let text_center_y = bg_y + rect.size.height / 2.0;
                        let baseline_y = text_center_y - (label_line.ascent - label_line.descent) / 2.0;

                        CGContext::save_g_state(Some(cg));
                        CGContext::set_text_position(
                            Some(cg),
                            workspace.label_x as CGFloat,
                            baseline_y as CGFloat,
                        );
                        let line_ref: &CTLine = label_line.line.as_ref();
                        unsafe { line_ref.draw(cg) };
                        CGContext::restore_g_state(Some(cg));
                    }
                }

                CGContext::restore_g_state(Some(cg));
            }
        }
    }
);

#[cfg(test)]
mod tests {
    use super::{
        APP_ICON_SIZE, APP_ICON_SPACING, CELL_SPACING, DISPLAY_GROUP_SPACING,
        LABEL_HORIZONTAL_INSET, inter_workspace_spacing, label_and_icon_positions,
        label_cell_width, label_fill_alpha,
    };

    #[test]
    fn label_cells_reserve_space_for_the_primary_app_icon() {
        let label_width = 7.0;
        assert_eq!(label_cell_width(label_width, false), 20.0);
        assert_eq!(
            label_cell_width(label_width, true),
            label_width + APP_ICON_SIZE + APP_ICON_SPACING + LABEL_HORIZONTAL_INSET * 2.0
        );
    }

    #[test]
    fn label_precedes_the_primary_app_icon() {
        let content_x = 5.0;
        let label_width = 7.0;
        let (label_x, icon_x) = label_and_icon_positions(content_x, label_width, true);
        assert_eq!(label_x, content_x);
        assert_eq!(icon_x, Some(content_x + label_width + APP_ICON_SPACING));
    }

    #[test]
    fn display_boundaries_are_wider_than_workspace_spacing() {
        assert_eq!(inter_workspace_spacing(false), CELL_SPACING);
        assert_eq!(inter_workspace_spacing(true), DISPLAY_GROUP_SPACING);
        assert!(DISPLAY_GROUP_SPACING > CELL_SPACING);
    }

    #[test]
    fn label_style_only_gives_the_active_workspace_a_subtle_background() {
        assert_eq!(label_fill_alpha(false), 0.0);
        let active_alpha = label_fill_alpha(true);
        assert!(active_alpha > 0.0);
        assert!(active_alpha < 0.2);
    }
}
