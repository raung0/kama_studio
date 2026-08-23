use super::*;




pub(super) const APP_MENU_HEIGHT: f32 = 22.0;
#[cfg(not(target_os = "macos"))]
const APP_MENU_FILE_WIDTH: f32 = 54.0;
#[cfg(not(target_os = "macos"))]
const APP_MENU_EDIT_WIDTH: f32 = 50.0;
#[cfg(not(target_os = "macos"))]
const APP_MENU_VIEW_WIDTH: f32 = 52.0;
#[cfg(not(target_os = "macos"))]
const APP_MENU_LAYOUT_WIDTH: f32 = 66.0;
#[cfg(not(target_os = "macos"))]
const APP_MENU_HELP_WIDTH: f32 = 52.0;
#[cfg(not(target_os = "macos"))]
const APP_MENU_ITEM_HEIGHT: f32 = 27.0;

#[cfg(not(target_os = "macos"))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum AppMenuState {
    #[default]
    Closed,
    File {
        recent: bool,
    },
    Edit,
    View,
    Layout {
        delete: bool,
    },
    Help,
}

#[cfg(not(target_os = "macos"))]
impl AppMenuState {
    pub(super) fn is_file(self) -> bool {
        matches!(self, Self::File { .. })
    }
    pub(super) fn is_edit(self) -> bool {
        self == Self::Edit
    }
    pub(super) fn is_view(self) -> bool {
        self == Self::View
    }
    pub(super) fn is_layout(self) -> bool {
        matches!(self, Self::Layout { .. })
    }
    pub(super) fn is_help(self) -> bool {
        self == Self::Help
    }
}

#[cfg(not(target_os = "macos"))]
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct AppMenuKeyboardState {
    pub(super) active: bool,
    pub(super) item: usize,
    pub(super) submenu_item: usize,
}

#[cfg(not(target_os = "macos"))]
pub(super) fn app_menu_top_index(state: AppMenuState) -> usize {
    match state {
        AppMenuState::File { .. } => 0,
        AppMenuState::Edit => 1,
        AppMenuState::View => 2,
        AppMenuState::Layout { .. } => 3,
        AppMenuState::Help => 4,
        AppMenuState::Closed => 0,
    }
}

#[cfg(not(target_os = "macos"))]
pub(super) fn app_menu_state_at(index: usize) -> AppMenuState {
    match index % 5 {
        0 => AppMenuState::File { recent: false },
        1 => AppMenuState::Edit,
        2 => AppMenuState::View,
        3 => AppMenuState::Layout { delete: false },
        _ => AppMenuState::Help,
    }
}

#[cfg(not(target_os = "macos"))]
pub(super) fn app_menu_button_rects() -> [Rect; 5] {
    let widths = [
        APP_MENU_FILE_WIDTH,
        APP_MENU_EDIT_WIDTH,
        APP_MENU_VIEW_WIDTH,
        APP_MENU_LAYOUT_WIDTH,
        APP_MENU_HELP_WIDTH,
    ];
    let viewport = Rect::new(0.0, 0.0, widths.iter().sum::<f32>() + 6.0, APP_MENU_HEIGHT);
    let (ids, measured) = ui::measure_layout(viewport, |ctx| {
        let mut ids = [ui::BlockId(0); 5];
        ctx.new()
            .row()
            .width(Size::Fill)
            .height(Size::Fill)
            .padding(2.0)
            .children(|ctx| {
                ctx.new()
                    .width(Size::Pixels(2.0))
                    .height(Size::Fill)
                    .build();
                for (index, width) in widths.into_iter().enumerate() {
                    ids[index] = ctx
                        .new()
                        .width(Size::Pixels(width))
                        .height(Size::Fill)
                        .build();
                }
            })
            .build();
        ids
    });
    ids.map(|id| measured.rect(id).expect("application menu button layout"))
}

#[cfg(not(target_os = "macos"))]
fn measured_menu_popup(x: f32, y: f32, width: f32, item_count: usize) -> (Rect, Vec<Rect>) {
    ui_layout::fit_column_at(
        Rect::new(0.0, 0.0, 4096.0, 4096.0),
        [x, y],
        width,
        &vec![ui_layout::Item::height(APP_MENU_ITEM_HEIGHT); item_count],
        0.0,
        5.0,
    )
}

#[cfg(not(target_os = "macos"))]
fn menu_button_rect(index: usize) -> Rect {
    app_menu_button_rects()[index]
}
#[cfg(not(target_os = "macos"))]
pub(super) fn file_menu_button_rect() -> Rect {
    menu_button_rect(0)
}
#[cfg(not(target_os = "macos"))]
pub(super) fn edit_menu_button_rect() -> Rect {
    menu_button_rect(1)
}
#[cfg(not(target_os = "macos"))]
pub(super) fn view_menu_button_rect() -> Rect {
    menu_button_rect(2)
}
#[cfg(not(target_os = "macos"))]
pub(super) fn layout_menu_button_rect() -> Rect {
    menu_button_rect(3)
}
#[cfg(not(target_os = "macos"))]
pub(super) fn help_menu_button_rect() -> Rect {
    menu_button_rect(4)
}

#[cfg(not(target_os = "macos"))]
pub(super) fn file_menu_popup_rect(has_latest: bool) -> Rect {
    let anchor = file_menu_button_rect();
    measured_menu_popup(
        anchor.x,
        APP_MENU_HEIGHT,
        196.0,
        if has_latest { 8 } else { 7 },
    )
    .0
}
#[cfg(not(target_os = "macos"))]
pub(super) fn recent_menu_popup_rect(recent_count: usize) -> Rect {
    let parent = file_menu_popup_rect(recent_count > 0);
    let anchor = menu_item_rect(parent, 4);
    measured_menu_popup(parent.right() - 2.0, anchor.y, 280.0, recent_count).0
}
#[cfg(not(target_os = "macos"))]
pub(super) fn edit_menu_popup_rect() -> Rect {
    let anchor = edit_menu_button_rect();
    measured_menu_popup(anchor.x, APP_MENU_HEIGHT, 224.0, EDIT_MENU_COMMANDS.len()).0
}
#[cfg(not(target_os = "macos"))]
pub(super) fn view_menu_popup_rect() -> Rect {
    let anchor = view_menu_button_rect();
    measured_menu_popup(anchor.x, APP_MENU_HEIGHT, 224.0, 1).0
}
#[cfg(not(target_os = "macos"))]
pub(super) fn layout_menu_popup_rect(layout_count: usize) -> Rect {
    let anchor = layout_menu_button_rect();
    measured_menu_popup(anchor.x, APP_MENU_HEIGHT, 206.0, layout_count + 3).0
}
#[cfg(not(target_os = "macos"))]
pub(super) fn delete_layout_menu_popup_rect(layout_count: usize) -> Rect {
    let parent = layout_menu_popup_rect(layout_count);
    let anchor = menu_item_rect(parent, layout_count + 2);
    measured_menu_popup(parent.right() - 2.0, anchor.y, 190.0, layout_count).0
}
#[cfg(not(target_os = "macos"))]
pub(super) fn help_menu_popup_rect() -> Rect {
    let anchor = help_menu_button_rect();
    measured_menu_popup(anchor.x, APP_MENU_HEIGHT, 204.0, 1).0
}
#[cfg(not(target_os = "macos"))]
fn menu_item_rect(popup: Rect, index: usize) -> Rect {
    measured_menu_popup(popup.x, popup.y, popup.width, index + 1).1[index]
}
#[cfg(not(target_os = "macos"))]
pub(super) fn recent_menu_item_rect(index: usize, recent_count: usize) -> Rect {
    menu_item_rect(recent_menu_popup_rect(recent_count), index)
}
#[cfg(not(target_os = "macos"))]
pub(super) fn edit_menu_item_rect(index: usize) -> Rect {
    menu_item_rect(edit_menu_popup_rect(), index)
}
#[cfg(not(target_os = "macos"))]
pub(super) fn view_menu_item_rect() -> Rect {
    menu_item_rect(view_menu_popup_rect(), 0)
}
#[cfg(not(target_os = "macos"))]
pub(super) fn help_menu_item_rect() -> Rect {
    menu_item_rect(help_menu_popup_rect(), 0)
}
#[cfg(not(target_os = "macos"))]
pub(super) fn file_menu_item_rect(index: usize, has_latest: bool) -> Rect {
    menu_item_rect(file_menu_popup_rect(has_latest), index)
}
#[cfg(not(target_os = "macos"))]
pub(super) fn layout_menu_item_rect(index: usize, layout_count: usize) -> Rect {
    menu_item_rect(layout_menu_popup_rect(layout_count), index)
}
#[cfg(not(target_os = "macos"))]
pub(super) fn delete_layout_menu_item_rect(index: usize, layout_count: usize) -> Rect {
    menu_item_rect(delete_layout_menu_popup_rect(layout_count), index)
}

pub(super) const EDIT_MENU_COMMANDS: &[&str] = &[
    "edit.undo",
    "edit.redo",
    "edit.copy",
    "edit.cut",
    "edit.paste",
    "timeline.close-gap",
    "timeline.speed-duration",
    "timeline.cut-at-playhead",
    "timeline.group-selection",
    "timeline.ungroup-selection",
    "timeline.delete-selection",
    #[cfg(not(target_os = "macos"))]
    "application.settings",
];

#[cfg(not(target_os = "macos"))]
pub(super) fn file_menu_commands(projects: &[PathBuf]) -> Vec<Option<FileCommand>> {
    let mut commands = vec![
        Some(FileCommand::NewProject),
        Some(FileCommand::Save),
        Some(FileCommand::SaveAs),
        Some(FileCommand::Load),
        None,
    ];
    if let Some(path) = projects.first() {
        commands.push(Some(FileCommand::LoadRecent(path.clone())));
    }
    commands.extend([Some(FileCommand::ImportMedia), Some(FileCommand::Exit)]);
    commands
}

#[cfg(not(target_os = "macos"))]
pub(super) fn build_button<K: Hash>(
    ctx: &mut ui::BuildCtx,
    key: K,
    rect: Rect,
    label: &str,
    open: bool,
    cursor: [f32; 2],
) {
    let highlighted = open || rect.contains(cursor);
    ui::ui!(ctx, {
        Rect(key, rect) {
            overlay;
            fill: if highlighted { theme::accent_hover() } else { Color::TRANSPARENT };
            border_radius: RADIUS_SM;
            font_size: 11.0;
            text_color: if highlighted { theme::accent_text() } else { theme::text() };
            text_centered;
            text: label;
            interactive;
        }
    });
}

#[cfg(not(target_os = "macos"))]
pub(super) fn build_popup<K: Hash>(ctx: &mut ui::BuildCtx, key: K, rect: Rect) {
    ui::ui!(ctx, {
        Rect(key, rect) {
            overlay; backdrop_blur: 22.0; backdrop_tint: theme::popup_tint();
            fill: theme::floating_bg(); border: 1; border_color: theme::accent(); border_radius: RADIUS_MD;
        }
    });
}

#[cfg(not(target_os = "macos"))]
#[allow(clippy::too_many_arguments)]
pub(super) fn build_item<K: Hash>(
    ctx: &mut ui::BuildCtx,
    key: K,
    rect: Rect,
    cursor: [f32; 2],
    keyboard_selected: bool,
    label: &str,
    shortcut: Option<&str>,
    icon: Option<AppIcon>,
    enabled: bool,
    font_size: f32,
    tooltip: Option<String>,
    icons: Icons,
) {
    let highlighted = enabled && (rect.contains(cursor) || keyboard_selected);
    let foreground = if highlighted {
        theme::accent_text()
    } else if enabled {
        theme::popup_text()
    } else {
        theme::popup_dim()
    };
    let muted = if highlighted {
        theme::accent_text()
    } else if enabled {
        theme::popup_muted()
    } else {
        theme::popup_dim()
    };
    let item = ctx.rect(key, rect).overlay().row()
        .fill(if highlighted { theme::accent_hover() } else { Color::TRANSPARENT })
        .border_radius(RADIUS_SM).padding(6.0).gap(7.0).interactive_if(enabled)
        .cursor(if enabled { CursorShape::Pointer } else { CursorShape::Passthrough })
        .children(|ctx| {
            ctx.new().width(Size::Pixels(20.0)).height(Size::Fill).content_centered().children(|ctx| {
                if let Some(icon) = icon {
                    ui::ui!(ctx, { Icon { icon!: icons.get(icon); color!: muted; width: Size::Pixels(20.0); height: Size::Pixels(20.0); } });
                }
            }).build();
            ctx.new().width(Size::Fill).height(Size::Fill).font_size(font_size).text_color(foreground)
                .text_vertical_align(ui::Align::Center).text(label).build();
            if let Some(shortcut) = shortcut {
                ctx.new().width(Size::Pixels(76.0)).height(Size::Fill).font_size((font_size - 1.0).max(8.0))
                    .text_color(muted).text_align(ui::Align::End).text_vertical_align(ui::Align::Center)
                    .text(shortcut).build();
            }
        });
    if let Some(tooltip) = tooltip {
        item.tooltip(tooltip).build();
    } else {
        item.build();
    }
}

#[cfg(not(target_os = "macos"))]
pub(super) fn command_meta(
    registry: &CommandRegistry,
    id: &str,
) -> (Option<AppIcon>, Option<String>) {
    registry
        .definition(id)
        .map(|definition| {
            (
                Some(definition.icon),
                definition.shortcut.map(|shortcut| shortcut.to_string()),
            )
        })
        .unwrap_or((None, None))
}

#[cfg(target_os = "macos")]
pub(super) struct NativeMenu {
    _menu: Menu,
    about: MenuItem,
    settings: MenuItem,
    quit: MenuItem,
    view_palette: MenuItem,
    new_project: MenuItem,
    save: MenuItem,
    save_as: MenuItem,
    load: MenuItem,
    import_media: MenuItem,
    exit: MenuItem,
    file_menu: Submenu,
    recent_menu: Submenu,
    recent_items: Vec<(MenuItem, PathBuf)>,
    latest_item: Option<(MenuItem, PathBuf)>,
    edit_items: Vec<(MenuItem, &'static str)>,
    layout_menu: Submenu,
    save_layout: MenuItem,
    restore_default_layout: MenuItem,
    delete_layout_menu: Submenu,
    layout_items: Vec<(MenuItem, PathBuf)>,
    delete_layout_items: Vec<(MenuItem, PathBuf)>,
}

#[cfg(target_os = "macos")]
impl NativeMenu {
    pub(super) fn install(commands: &CommandRegistry) -> Self {
        let menu = Menu::new();

        let app_menu = Submenu::new("Kama Studio", true);
        let about = MenuItem::new("About Kama Studio…", true, None);
        let settings = MenuItem::new(
            "Settings…",
            true,
            Some(Accelerator::new(Some(Modifiers::META), Code::Comma)),
        );
        let quit = MenuItem::new(
            "Quit Kama Studio",
            true,
            Some(Accelerator::new(Some(Modifiers::META), Code::KeyQ)),
        );
        app_menu
            .append_items(&[
                &about,
                &PredefinedMenuItem::separator(),
                &settings,
                &PredefinedMenuItem::separator(),
                &PredefinedMenuItem::services(None),
                &PredefinedMenuItem::separator(),
                &PredefinedMenuItem::hide(None),
                &PredefinedMenuItem::hide_others(None),
                &PredefinedMenuItem::show_all(None),
                &PredefinedMenuItem::separator(),
                &quit,
            ])
            .expect("append native application menu items");
        menu.append(&app_menu)
            .expect("append native application menu");

        let file_menu = Submenu::new("File", true);
        let new_project = MenuItem::new(
            "New Project",
            true,
            Some(Accelerator::new(Some(Modifiers::META), Code::KeyN)),
        );
        let save = MenuItem::new(
            "Save",
            true,
            Some(Accelerator::new(Some(Modifiers::META), Code::KeyS)),
        );
        let save_as = MenuItem::new(
            "Save As…",
            true,
            Some(Accelerator::new(
                Some(Modifiers::META | Modifiers::SHIFT),
                Code::KeyS,
            )),
        );
        let load = MenuItem::new(
            "Open Project…",
            true,
            Some(Accelerator::new(Some(Modifiers::META), Code::KeyO)),
        );
        let recent_menu = Submenu::new("Open Recent Project", false);
        let import_media = MenuItem::new(
            "Import Media…",
            true,
            Some(Accelerator::new(Some(Modifiers::META), Code::KeyI)),
        );
        let exit = MenuItem::new("Exit", true, None);
        file_menu
            .append_items(&[
                &new_project,
                &PredefinedMenuItem::separator(),
                &save,
                &save_as,
                &PredefinedMenuItem::separator(),
                &load,
                &recent_menu,
                &import_media,
                &PredefinedMenuItem::separator(),
                &exit,
            ])
            .expect("append native File menu items");
        menu.append(&file_menu).expect("append native File menu");

        let edit_menu = Submenu::new("Edit", true);
        let mut edit_items = Vec::with_capacity(EDIT_MENU_COMMANDS.len());
        for &command in EDIT_MENU_COMMANDS {
            let accelerator = match command {
                "edit.undo" => Some(Accelerator::new(Some(Modifiers::META), Code::KeyZ)),
                "edit.redo" => Some(Accelerator::new(
                    Some(Modifiers::META | Modifiers::SHIFT),
                    Code::KeyZ,
                )),
                "edit.copy" => Some(Accelerator::new(Some(Modifiers::META), Code::KeyC)),
                "edit.cut" => Some(Accelerator::new(Some(Modifiers::META), Code::KeyX)),
                "edit.paste" => Some(Accelerator::new(Some(Modifiers::META), Code::KeyV)),
                "timeline.cut-at-playhead" => {
                    Some(Accelerator::new(Some(Modifiers::META), Code::KeyK))
                }
                _ => None,
            };
            let label = commands
                .definition(command)
                .map(|definition| definition.label.as_str())
                .unwrap_or(command);
            let item = MenuItem::new(label, true, accelerator);
            edit_menu
                .append(&item)
                .expect("append native Edit menu item");
            edit_items.push((item, command));
        }
        menu.append(&edit_menu).expect("append native Edit menu");

        let view_menu = Submenu::new("View", true);
        let view_palette = MenuItem::new(
            "Command Palette…",
            true,
            Some(Accelerator::new(Some(Modifiers::META), Code::KeyP)),
        );
        view_menu
            .append(&view_palette)
            .expect("append native View menu item");
        menu.append(&view_menu).expect("append native View menu");

        let layout_menu = Submenu::new("Layout", true);
        let save_layout = MenuItem::new("Save Layout…", true, None);
        let restore_default_layout = MenuItem::new("Restore Default Layout", true, None);
        let delete_layout_menu = Submenu::new("Delete Layout", false);
        layout_menu
            .append_items(&[
                &save_layout,
                &PredefinedMenuItem::separator(),
                &restore_default_layout,
                &delete_layout_menu,
            ])
            .expect("append native Layout menu items");
        menu.append(&layout_menu)
            .expect("append native Layout menu");
        menu.init_for_nsapp();

        let mut native = Self {
            _menu: menu,
            about,
            settings,
            quit,
            view_palette,
            new_project,
            save,
            save_as,
            load,
            import_media,
            exit,
            file_menu,
            recent_menu,
            recent_items: Vec::new(),
            latest_item: None,
            edit_items,
            layout_menu,
            save_layout,
            restore_default_layout,
            delete_layout_menu,
            layout_items: Vec::new(),
            delete_layout_items: Vec::new(),
        };
        native.refresh_layouts();
        native.refresh_recent_projects();
        native
    }

    pub(super) fn about_requested(&self, event: &MenuEvent) -> bool {
        event.id == self.about.id()
    }

    pub(super) fn settings_requested(&self, event: &MenuEvent) -> bool {
        event.id == self.settings.id()
    }

    pub(super) fn view_command(&self, event: &MenuEvent) -> Option<&'static str> {
        (event.id == self.view_palette.id()).then_some("application.command-palette")
    }

    pub(super) fn file_command(&self, event: &MenuEvent) -> Option<FileCommand> {
        if event.id == self.new_project.id() {
            Some(FileCommand::NewProject)
        } else if event.id == self.save.id() {
            Some(FileCommand::Save)
        } else if event.id == self.save_as.id() {
            Some(FileCommand::SaveAs)
        } else if event.id == self.load.id() {
            Some(FileCommand::Load)
        } else if event.id == self.import_media.id() {
            Some(FileCommand::ImportMedia)
        } else if event.id == self.exit.id() || event.id == self.quit.id() {
            Some(FileCommand::Exit)
        } else if let Some((item, path)) = &self.latest_item {
            if event.id == item.id() {
                return Some(FileCommand::LoadRecent(path.clone()));
            }
            self.recent_items
                .iter()
                .find(|(item, _)| event.id == item.id())
                .map(|(_, path)| FileCommand::LoadRecent(path.clone()))
        } else {
            self.recent_items
                .iter()
                .find(|(item, _)| event.id == item.id())
                .map(|(_, path)| FileCommand::LoadRecent(path.clone()))
        }
    }

    pub(super) fn edit_command(&self, event: &MenuEvent) -> Option<&'static str> {
        self.edit_items
            .iter()
            .find(|(item, _)| event.id == item.id())
            .map(|(_, command)| *command)
    }

    pub(super) fn refresh_recent_projects(&mut self) {
        if let Some((item, _)) = self.latest_item.take() {
            let _ = self.file_menu.remove(&item);
        }
        for (item, _) in self.recent_items.drain(..) {
            let _ = self.recent_menu.remove(&item);
        }
        let recent = recent_projects();
        for path in &recent {
            let label = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("Project");
            let item = MenuItem::new(label, true, None);
            self.recent_menu
                .append(&item)
                .expect("append recent project menu item");
            self.recent_items.push((item, path.clone()));
        }
        self.recent_menu.set_enabled(!self.recent_items.is_empty());

        if let Some(path) = recent.first() {
            let label = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("Project");
            let item = MenuItem::new(format!("Open {label}"), true, None);
            self.file_menu
                .insert(&item, 5)
                .expect("insert latest project menu item");
            self.latest_item = Some((item, path.clone()));
        }
    }

    pub(super) fn refresh_layouts(&mut self) {
        for (item, _) in self.layout_items.drain(..) {
            let _ = self.layout_menu.remove(&item);
        }
        for (item, _) in self.delete_layout_items.drain(..) {
            let _ = self.delete_layout_menu.remove(&item);
        }
        for (index, layout) in saved_layouts().into_iter().enumerate() {
            let load = MenuItem::new(&layout.name, true, None);
            self.layout_menu
                .insert(&load, 1 + index)
                .expect("insert saved layout menu item");
            self.layout_items.push((load, layout.path.clone()));

            let delete = MenuItem::new(&layout.name, true, None);
            self.delete_layout_menu
                .append(&delete)
                .expect("append delete layout menu item");
            self.delete_layout_items.push((delete, layout.path));
        }
        self.delete_layout_menu
            .set_enabled(!self.delete_layout_items.is_empty());
    }

    pub(super) fn layout_command(&self, event: &MenuEvent) -> Option<LayoutCommand> {
        if event.id == self.save_layout.id() {
            Some(LayoutCommand::Save)
        } else if event.id == self.restore_default_layout.id() {
            Some(LayoutCommand::RestoreDefault)
        } else if let Some((_, path)) = self
            .layout_items
            .iter()
            .find(|(item, _)| event.id == item.id())
        {
            Some(LayoutCommand::Load(path.clone()))
        } else {
            self.delete_layout_items
                .iter()
                .find(|(item, _)| event.id == item.id())
                .map(|(_, path)| LayoutCommand::Delete(path.clone()))
        }
    }
}

#[cfg(not(target_os = "macos"))]
impl EditorApp {
    #[cfg(not(target_os = "macos"))]
    pub(super) fn app_menu_parent_item_count(&self) -> usize {
        match self.app_menu {
            AppMenuState::File { .. } => file_menu_commands(&recent_projects()).len(),
            AppMenuState::Edit => EDIT_MENU_COMMANDS.len(),
            AppMenuState::View | AppMenuState::Help => 1,
            AppMenuState::Layout { .. } => saved_layouts().len() + 3,
            AppMenuState::Closed => 0,
        }
    }

    #[cfg(not(target_os = "macos"))]
    pub(super) fn activate_app_menu_keyboard_item(&mut self) {
        match self.app_menu {
            AppMenuState::File { recent: true } => {
                let projects = recent_projects();
                if let Some(path) = projects.get(self.app_menu_keyboard.submenu_item).cloned() {
                    self.app_menu = AppMenuState::Closed;
                    self.app_menu_keyboard.active = false;
                    self.handle_file_command(FileCommand::LoadRecent(path));
                }
            }
            AppMenuState::File { recent: false } => {
                let projects = recent_projects();
                let commands = file_menu_commands(&projects);
                if self.app_menu_keyboard.item == 4 {
                    if !projects.is_empty() {
                        self.app_menu = AppMenuState::File { recent: true };
                        self.app_menu_keyboard.submenu_item = 0;
                    }
                } else if let Some(Some(command)) =
                    commands.get(self.app_menu_keyboard.item).cloned()
                {
                    self.app_menu = AppMenuState::Closed;
                    self.app_menu_keyboard.active = false;
                    self.handle_file_command(command);
                }
            }
            AppMenuState::Edit => {
                if let Some(command) = EDIT_MENU_COMMANDS
                    .get(self.app_menu_keyboard.item)
                    .and_then(|id| self.command_registry.command(id))
                {
                    self.app_menu = AppMenuState::Closed;
                    self.app_menu_keyboard.active = false;
                    self.command_queue.push(command);
                }
            }
            AppMenuState::View => {
                self.app_menu = AppMenuState::Closed;
                self.app_menu_keyboard.active = false;
                if let Some(command) = self.command_registry.command("application.command-palette")
                {
                    self.command_queue.push(command);
                }
            }
            AppMenuState::Layout { delete: true } => {
                let layouts = saved_layouts();
                if let Some(layout) = layouts.get(self.app_menu_keyboard.submenu_item) {
                    let path = layout.path.clone();
                    self.app_menu = AppMenuState::Closed;
                    self.app_menu_keyboard.active = false;
                    self.handle_layout_command(LayoutCommand::Delete(path));
                }
            }
            AppMenuState::Layout { delete: false } => {
                let layouts = saved_layouts();
                let count = layouts.len();
                let selected = self.app_menu_keyboard.item;
                if selected == 0 {
                    self.app_menu = AppMenuState::Closed;
                    self.app_menu_keyboard.active = false;
                    self.handle_layout_command(LayoutCommand::Save);
                } else if selected <= count {
                    let path = layouts[selected - 1].path.clone();
                    self.app_menu = AppMenuState::Closed;
                    self.app_menu_keyboard.active = false;
                    self.handle_layout_command(LayoutCommand::Load(path));
                } else if selected == count + 1 {
                    self.app_menu = AppMenuState::Closed;
                    self.app_menu_keyboard.active = false;
                    self.handle_layout_command(LayoutCommand::RestoreDefault);
                } else if selected == count + 2 && count > 0 {
                    self.app_menu = AppMenuState::Layout { delete: true };
                    self.app_menu_keyboard.submenu_item = 0;
                }
            }
            AppMenuState::Help => {
                self.app_menu = AppMenuState::Closed;
                self.app_menu_keyboard.active = false;
                self.open_modal(Modal::About(SimpleDialog::new()));
            }
            AppMenuState::Closed => {}
        }
    }

    #[cfg(not(target_os = "macos"))]
    pub(super) fn handle_app_menu_key(&mut self, event: &KeyEvent) -> bool {
        if event.state != ElementState::Pressed {
            return !matches!(self.app_menu, AppMenuState::Closed);
        }

        if matches!(self.app_menu, AppMenuState::Closed) {
            if matches!(event.logical_key, Key::Named(NamedKey::F10 | NamedKey::Alt)) {
                self.app_menu = AppMenuState::File { recent: false };
                self.app_menu_keyboard = AppMenuKeyboardState {
                    active: true,
                    item: 0,
                    submenu_item: 0,
                };
                return true;
            }
            return false;
        }

        self.app_menu_keyboard.active = true;
        match event.logical_key {
            Key::Named(NamedKey::Escape | NamedKey::Alt) => {
                self.app_menu = AppMenuState::Closed;
                self.app_menu_keyboard.active = false;
            }
            Key::Named(NamedKey::ArrowRight) => {
                let opens_submenu = match self.app_menu {
                    AppMenuState::File { recent: false } => {
                        self.app_menu_keyboard.item == 4 && !recent_projects().is_empty()
                    }
                    AppMenuState::Layout { delete: false } => {
                        let count = saved_layouts().len();
                        self.app_menu_keyboard.item == count + 2 && count > 0
                    }
                    _ => false,
                };
                if opens_submenu {
                    match self.app_menu {
                        AppMenuState::File { .. } => {
                            self.app_menu = AppMenuState::File { recent: true }
                        }
                        AppMenuState::Layout { .. } => {
                            self.app_menu = AppMenuState::Layout { delete: true }
                        }
                        _ => {}
                    }
                    self.app_menu_keyboard.submenu_item = 0;
                } else {
                    let next = (app_menu_top_index(self.app_menu) + 1) % 5;
                    self.app_menu = app_menu_state_at(next);
                    self.app_menu_keyboard.item = 0;
                    self.app_menu_keyboard.submenu_item = 0;
                }
            }
            Key::Named(NamedKey::ArrowLeft) => {
                if matches!(self.app_menu, AppMenuState::File { recent: true }) {
                    self.app_menu = AppMenuState::File { recent: false };
                } else if matches!(self.app_menu, AppMenuState::Layout { delete: true }) {
                    self.app_menu = AppMenuState::Layout { delete: false };
                } else {
                    let current = app_menu_top_index(self.app_menu);
                    self.app_menu = app_menu_state_at((current + 4) % 5);
                    self.app_menu_keyboard.item = 0;
                }
                self.app_menu_keyboard.submenu_item = 0;
            }
            Key::Named(NamedKey::ArrowDown) => {
                let submenu_count = match self.app_menu {
                    AppMenuState::File { recent: true } => recent_projects().len(),
                    AppMenuState::Layout { delete: true } => saved_layouts().len(),
                    _ => 0,
                };
                if submenu_count > 0 {
                    self.app_menu_keyboard.submenu_item =
                        (self.app_menu_keyboard.submenu_item + 1) % submenu_count;
                } else {
                    let count = self.app_menu_parent_item_count();
                    if count > 0 {
                        self.app_menu_keyboard.item = (self.app_menu_keyboard.item + 1) % count;
                    }
                }
            }
            Key::Named(NamedKey::ArrowUp) => {
                let submenu_count = match self.app_menu {
                    AppMenuState::File { recent: true } => recent_projects().len(),
                    AppMenuState::Layout { delete: true } => saved_layouts().len(),
                    _ => 0,
                };
                if submenu_count > 0 {
                    self.app_menu_keyboard.submenu_item =
                        (self.app_menu_keyboard.submenu_item + submenu_count - 1) % submenu_count;
                } else {
                    let count = self.app_menu_parent_item_count();
                    if count > 0 {
                        self.app_menu_keyboard.item =
                            (self.app_menu_keyboard.item + count - 1) % count;
                    }
                }
            }
            Key::Named(NamedKey::Enter) => self.activate_app_menu_keyboard_item(),
            Key::Named(NamedKey::F10) => {
                self.app_menu = AppMenuState::Closed;
                self.app_menu_keyboard.active = false;
            }
            _ => {}
        }
        true
    }

    #[cfg(not(target_os = "macos"))]
    pub(super) fn hover_app_menu(&mut self) -> bool {
        if matches!(self.app_menu, AppMenuState::Closed) {
            return false;
        }

        for (index, rect) in app_menu_button_rects().into_iter().enumerate() {
            if rect.contains(self.cursor) {
                let next = app_menu_state_at(index);
                if std::mem::discriminant(&self.app_menu) != std::mem::discriminant(&next) {
                    self.app_menu = next;
                    self.app_menu_keyboard.item = 0;
                    self.app_menu_keyboard.submenu_item = 0;
                }
                self.app_menu_keyboard.active = false;
                return true;
            }
        }

        match self.app_menu {
            AppMenuState::File { recent } => {
                let projects = recent_projects();
                if recent {
                    if let Some(index) = (0..projects.len()).find(|index| {
                        recent_menu_item_rect(*index, projects.len()).contains(self.cursor)
                    }) {
                        self.app_menu_keyboard.submenu_item = index;
                        self.app_menu_keyboard.active = false;
                        return true;
                    }
                }
                if let Some(index) = (0..file_menu_commands(&projects).len()).find(|index| {
                    file_menu_item_rect(*index, !projects.is_empty()).contains(self.cursor)
                }) {
                    self.app_menu_keyboard.item = index;
                    self.app_menu_keyboard.active = false;
                    self.app_menu = AppMenuState::File {
                        recent: index == 4 && !projects.is_empty(),
                    };
                    return true;
                }
            }
            AppMenuState::Layout { delete } => {
                let count = saved_layouts().len();
                if delete {
                    if let Some(index) = (0..count).find(|index| {
                        delete_layout_menu_item_rect(*index, count).contains(self.cursor)
                    }) {
                        self.app_menu_keyboard.submenu_item = index;
                        self.app_menu_keyboard.active = false;
                        return true;
                    }
                }
                if let Some(index) = (0..count + 3)
                    .find(|index| layout_menu_item_rect(*index, count).contains(self.cursor))
                {
                    self.app_menu_keyboard.item = index;
                    self.app_menu_keyboard.active = false;
                    self.app_menu = AppMenuState::Layout {
                        delete: index == count + 2 && count > 0,
                    };
                    return true;
                }
            }
            AppMenuState::Edit => {
                if let Some(index) = (0..EDIT_MENU_COMMANDS.len())
                    .find(|index| edit_menu_item_rect(*index).contains(self.cursor))
                {
                    self.app_menu_keyboard.item = index;
                    self.app_menu_keyboard.active = false;
                    return true;
                }
            }
            AppMenuState::View => {
                if view_menu_item_rect().contains(self.cursor) {
                    self.app_menu_keyboard.item = 0;
                    self.app_menu_keyboard.active = false;
                    return true;
                }
            }
            AppMenuState::Help => {
                if help_menu_item_rect().contains(self.cursor) {
                    self.app_menu_keyboard.item = 0;
                    self.app_menu_keyboard.active = false;
                    return true;
                }
            }
            AppMenuState::Closed => {}
        }
        false
    }

    #[cfg(not(target_os = "macos"))]
    pub(super) fn handle_app_menu_pointer(&mut self) -> bool {
        for (rect, menu) in [
            (
                file_menu_button_rect(),
                AppMenuState::File { recent: false },
            ),
            (edit_menu_button_rect(), AppMenuState::Edit),
            (view_menu_button_rect(), AppMenuState::View),
            (
                layout_menu_button_rect(),
                AppMenuState::Layout { delete: false },
            ),
            (help_menu_button_rect(), AppMenuState::Help),
        ] {
            if rect.contains(self.cursor) {
                self.app_menu_keyboard.active = false;
                self.app_menu_keyboard.item = 0;
                self.app_menu_keyboard.submenu_item = 0;
                self.app_menu =
                    if std::mem::discriminant(&self.app_menu) == std::mem::discriminant(&menu) {
                        AppMenuState::Closed
                    } else {
                        menu
                    };
                return true;
            }
        }

        match self.app_menu {
            AppMenuState::File { recent } => {
                let projects = recent_projects();
                if recent {
                    if let Some(path) = projects.iter().enumerate().find_map(|(index, path)| {
                        recent_menu_item_rect(index, projects.len())
                            .contains(self.cursor)
                            .then(|| path.clone())
                    }) {
                        self.app_menu = AppMenuState::Closed;
                        self.handle_file_command(FileCommand::LoadRecent(path));
                        return true;
                    }
                }
                let commands = file_menu_commands(&projects);
                if let Some((_index, command)) =
                    commands.into_iter().enumerate().find(|(index, _)| {
                        file_menu_item_rect(*index, !projects.is_empty()).contains(self.cursor)
                    })
                {
                    if let Some(command) = command {
                        self.app_menu = AppMenuState::Closed;
                        self.handle_file_command(command);
                    } else if !projects.is_empty() {
                        self.app_menu = AppMenuState::File { recent: !recent };
                    }
                    return true;
                }
                if file_menu_popup_rect(!projects.is_empty()).contains(self.cursor)
                    || (recent && recent_menu_popup_rect(projects.len()).contains(self.cursor))
                {
                    return true;
                }
                self.app_menu = AppMenuState::Closed;
            }
            AppMenuState::Edit => {
                if let Some(command) = EDIT_MENU_COMMANDS
                    .iter()
                    .enumerate()
                    .find(|(index, _)| edit_menu_item_rect(*index).contains(self.cursor))
                    .and_then(|(_, id)| self.command_registry.command(id))
                {
                    self.app_menu = AppMenuState::Closed;
                    self.command_queue.push(command);
                    return true;
                }
                if edit_menu_popup_rect().contains(self.cursor) {
                    return true;
                }
                self.app_menu = AppMenuState::Closed;
            }
            AppMenuState::View => {
                if view_menu_item_rect().contains(self.cursor) {
                    self.app_menu = AppMenuState::Closed;
                    if let Some(command) =
                        self.command_registry.command("application.command-palette")
                    {
                        self.command_queue.push(command);
                    }
                    return true;
                }
                if view_menu_popup_rect().contains(self.cursor) {
                    return true;
                }
                self.app_menu = AppMenuState::Closed;
            }
            AppMenuState::Layout { delete } => {
                let layouts = saved_layouts();
                let count = layouts.len();
                if delete {
                    if let Some(layout) = layouts.iter().enumerate().find_map(|(index, layout)| {
                        delete_layout_menu_item_rect(index, count)
                            .contains(self.cursor)
                            .then(|| layout.clone())
                    }) {
                        self.app_menu = AppMenuState::Closed;
                        self.handle_layout_command(LayoutCommand::Delete(layout.path));
                        return true;
                    }
                }
                if layout_menu_item_rect(0, count).contains(self.cursor) {
                    self.app_menu = AppMenuState::Closed;
                    self.handle_layout_command(LayoutCommand::Save);
                    return true;
                }
                if let Some(layout) = layouts.iter().enumerate().find_map(|(index, layout)| {
                    layout_menu_item_rect(index + 1, count)
                        .contains(self.cursor)
                        .then(|| layout.clone())
                }) {
                    self.app_menu = AppMenuState::Closed;
                    self.handle_layout_command(LayoutCommand::Load(layout.path));
                    return true;
                }
                if layout_menu_item_rect(count + 1, count).contains(self.cursor) {
                    self.app_menu = AppMenuState::Closed;
                    self.handle_layout_command(LayoutCommand::RestoreDefault);
                    return true;
                }
                if count > 0 && layout_menu_item_rect(count + 2, count).contains(self.cursor) {
                    self.app_menu = AppMenuState::Layout { delete: !delete };
                    return true;
                }
                if layout_menu_popup_rect(count).contains(self.cursor)
                    || (delete && delete_layout_menu_popup_rect(count).contains(self.cursor))
                {
                    return true;
                }
                self.app_menu = AppMenuState::Closed;
            }
            AppMenuState::Help => {
                if help_menu_item_rect().contains(self.cursor) {
                    self.app_menu = AppMenuState::Closed;
                    self.open_modal(Modal::About(SimpleDialog::new()));
                    return true;
                }
                if help_menu_popup_rect().contains(self.cursor) {
                    return true;
                }
                self.app_menu = AppMenuState::Closed;
            }
            AppMenuState::Closed => {}
        }

        self.cursor[1] < APP_MENU_HEIGHT
    }
}

#[cfg(not(target_os = "macos"))]
pub(super) fn build_app_menu(
    ctx: &mut ui::BuildCtx,
    width: f32,
    state: AppMenuState,
    keyboard: AppMenuKeyboardState,
    cursor: [f32; 2],
    registry: &CommandRegistry,
    icons: Icons,
) {
    let file_open = state.is_file();
    let edit_open = state.is_edit();
    let view_open = state.is_view();
    let layout_open = state.is_layout();
    let help_open = state.is_help();
    let recent_open = matches!(state, AppMenuState::File { recent: true });
    let delete_layout_open = matches!(state, AppMenuState::Layout { delete: true });
    ctx.new()
        .id("app-menu-bar")
        .overlay()
        .position((0.0, 0.0))
        .width(Size::Pixels(width))
        .height(Size::Pixels(APP_MENU_HEIGHT))
        .fill(theme::tab_bar())
        .border(1)
        .border_color(theme::line())
        .build();

    build_button(
        ctx,
        "app-menu-file-button",
        file_menu_button_rect(),
        "File",
        file_open,
        cursor,
    );
    build_button(
        ctx,
        "app-menu-edit-button",
        edit_menu_button_rect(),
        "Edit",
        edit_open,
        cursor,
    );
    build_button(
        ctx,
        "app-menu-view-button",
        view_menu_button_rect(),
        "View",
        view_open,
        cursor,
    );
    build_button(
        ctx,
        "app-menu-layout-button",
        layout_menu_button_rect(),
        "Layout",
        layout_open,
        cursor,
    );
    build_button(
        ctx,
        "app-menu-help-button",
        help_menu_button_rect(),
        "Help",
        help_open,
        cursor,
    );

    if file_open {
        let recent = recent_projects();
        let popup = file_menu_popup_rect(!recent.is_empty());
        build_popup(ctx, "app-file-menu-popup", popup);

        let mut items = vec![
            ("New Project".to_string(), Some("project.new"), true, None),
            ("Save".to_string(), Some("project.save"), true, None),
            ("Save As…".to_string(), Some("project.save-as"), true, None),
            (
                "Open Project…".to_string(),
                Some("project.open"),
                true,
                None,
            ),
            (
                if recent.is_empty() {
                    "Open Recent Project"
                } else {
                    "Open Recent Project  ▸"
                }
                .to_string(),
                None,
                !recent.is_empty(),
                Some(AppIcon::Open),
            ),
        ];
        if let Some(path) = recent.first() {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("Project");
            items.push((format!("Open {name}"), None, true, Some(AppIcon::Open)));
        }
        items.extend([
            (
                "Import Media…".to_string(),
                Some("media.import"),
                true,
                None,
            ),
            ("Exit".to_string(), Some("application.exit"), true, None),
        ]);

        for (index, (label, command, enabled, fallback_icon)) in items.into_iter().enumerate() {
            let item = file_menu_item_rect(index, !recent.is_empty());
            let (icon, shortcut) = command
                .map(|id| command_meta(registry, id))
                .unwrap_or((fallback_icon, None));
            build_item(
                ctx,
                ("app-file-menu-item", index),
                item,
                cursor,
                keyboard.active && keyboard.item == index,
                &label,
                shortcut.as_deref(),
                icon.or(fallback_icon),
                enabled,
                10.5,
                None,
                icons,
            );
        }
        if recent_open && !recent.is_empty() {
            let recent_popup = recent_menu_popup_rect(recent.len());
            build_popup(ctx, "app-recent-menu-popup", recent_popup);
            for (index, path) in recent.iter().enumerate() {
                let item = recent_menu_item_rect(index, recent.len());
                let label = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("Project");
                build_item(
                    ctx,
                    ("app-recent-menu-item", index),
                    item,
                    cursor,
                    keyboard.active && keyboard.submenu_item == index,
                    label,
                    None,
                    Some(AppIcon::Open),
                    true,
                    10.5,
                    Some(path.display().to_string()),
                    icons,
                );
            }
        }
    }

    if edit_open {
        let popup = edit_menu_popup_rect();
        build_popup(ctx, "app-edit-menu-popup", popup);
        for (index, command) in EDIT_MENU_COMMANDS.iter().enumerate() {
            let item = edit_menu_item_rect(index);
            let (icon, shortcut) = command_meta(registry, command);
            let label = registry
                .definition(command)
                .map(|definition| definition.label.as_str())
                .unwrap_or(command);
            build_item(
                ctx,
                ("app-edit-menu-item", index),
                item,
                cursor,
                keyboard.active && keyboard.item == index,
                label,
                shortcut.as_deref(),
                icon,
                true,
                10.0,
                None,
                icons,
            );
        }
    }

    if view_open {
        let popup = view_menu_popup_rect();
        build_popup(ctx, "app-view-menu-popup", popup);
        let (icon, shortcut) = command_meta(registry, "application.command-palette");
        build_item(
            ctx,
            "app-view-command-palette",
            view_menu_item_rect(),
            cursor,
            keyboard.active && keyboard.item == 0,
            "Command Palette…",
            shortcut.as_deref(),
            icon,
            true,
            10.0,
            None,
            icons,
        );
    }

    if layout_open {
        let layouts = saved_layouts();
        let count = layouts.len();
        let popup = layout_menu_popup_rect(count);
        build_popup(ctx, "app-layout-menu-popup", popup);
        let mut items = vec![("Save Layout…".to_string(), None, true)];
        items.extend(
            layouts
                .iter()
                .map(|layout| (layout.name.clone(), Some(AppIcon::Open), true)),
        );
        items.push(("Restore Default Layout".to_string(), None, true));
        items.push((
            if count == 0 {
                "Delete Layout"
            } else {
                "Delete Layout  ▸"
            }
            .into(),
            Some(AppIcon::Delete),
            count > 0,
        ));
        for (index, (label, icon, enabled)) in items.into_iter().enumerate() {
            let item = layout_menu_item_rect(index, count);
            build_item(
                ctx,
                ("app-layout-menu-item", index),
                item,
                cursor,
                keyboard.active && keyboard.item == index,
                &label,
                None,
                icon,
                enabled,
                10.5,
                None,
                icons,
            );
        }
        if delete_layout_open && count > 0 {
            let delete_popup = delete_layout_menu_popup_rect(count);
            build_popup(ctx, "app-delete-layout-menu-popup", delete_popup);
            for (index, layout) in layouts.iter().enumerate() {
                let item = delete_layout_menu_item_rect(index, count);
                build_item(
                    ctx,
                    ("app-delete-layout-menu-item", index),
                    item,
                    cursor,
                    keyboard.active && keyboard.submenu_item == index,
                    &layout.name,
                    None,
                    Some(AppIcon::Delete),
                    true,
                    10.5,
                    None,
                    icons,
                );
            }
        }
    }

    if help_open {
        let popup = help_menu_popup_rect();
        build_popup(ctx, "app-help-menu-popup", popup);
        build_item(
            ctx,
            "app-help-about",
            help_menu_item_rect(),
            cursor,
            keyboard.active && keyboard.item == 0,
            "About Kama Studio…",
            None,
            Some(AppIcon::Inspector),
            true,
            10.5,
            None,
            icons,
        );
    }
}

pub(super) fn app_menu_height() -> f32 {
    if cfg!(target_os = "macos") {
        0.0
    } else {
        APP_MENU_HEIGHT
    }
}

pub(super) fn dock_tab_close_rect(tab: Rect) -> Rect {
    ui_layout::row(
        tab,
        &[ui_layout::Item::width(18.0), ui_layout::Item::fill()],
        2.0,
        2.0,
        ui::Align::Start,
    )[0]
}
