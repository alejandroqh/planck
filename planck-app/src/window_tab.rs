use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{
        Arc,
        mpsc::{Sender, channel},
    },
    time::Instant,
};

use alacritty_terminal::vte::ansi::Handler;
use floem::{
    ViewId,
    action::{TimerToken, open_file, remove_overlay, save_as},
    ext_event::{create_ext_action, create_signal_from_channel},
    file::{FileDialogOptions, FileSpec},
    keyboard::Modifiers,
    kurbo::Size,
    peniko::kurbo::{Point, Rect, Vec2},
    prelude::SignalTrack,
    reactive::{
        Memo, ReadSignal, RwSignal, Scope, SignalGet, SignalUpdate, SignalWith,
        WriteSignal, use_context,
    },
    text::{Attrs, AttrsList, FamilyOwned, LineHeightValue, TextLayout},
    views::editor::core::buffer::rope_text::RopeText,
};
use indexmap::IndexMap;
use itertools::Itertools;
use lsp_types::{
    CodeActionOrCommand, CodeLens, Diagnostic, ProgressParams, ProgressToken,
    ShowMessageParams,
};
use notify::Watcher;
use planck_core::{
    command::FocusCommand, cursor::CursorAffinity, directory::Directory, mode::Mode,
    register::Register,
};
use planck_rpc::{
    RpcError,
    core::CoreNotification,
    file::{Naming, PathObject},
    plugin::PluginId,
    proxy::{ProxyResponse, ProxyRpcHandler, ProxyStatus},
    run::{ConfigSource, RunConfig},
    source_control::FileDiff,
    terminal::TermId,
};
use serde_json::Value;
use tracing::{debug, error};

use crate::{
    about::AboutData,
    alert::{AlertBoxData, AlertButton},
    code_action::{CodeActionData, CodeActionStatus},
    command::{
        CommandExecuted, CommandKind, InternalCommand, PlanckCommand,
        PlanckWorkbenchCommand, WindowCommand,
    },
    completion::{CompletionData, CompletionStatus},
    config::{PlanckConfig, watcher::ConfigWatcher},
    db::PlanckDb,
    doc::DocContent,
    editor::{
        InlineFindDirection,
        location::{EditorLocation, EditorPosition},
    },
    editor_tab::EditorTabChild,
    file_explorer::data::FileExplorerData,
    find::Find,
    global_search::GlobalSearchData,
    hover::HoverData,
    id::WindowTabId,
    inline_completion::InlineCompletionData,
    keypress::{EventRef, KeyPressData, KeyPressFocus, condition::Condition},
    listener::Listener,
    lsp::path_from_url,
    main_split::{
        CarriedTabs, MainSplitData, SplitData, SplitDirection, SplitInfo,
        SplitMoveDirection,
    },
    palette::{DEFAULT_RUN_TOML, PaletteData, PaletteStatus, kind::PaletteKind},
    panel::{
        call_hierarchy_view::{CallHierarchyData, CallHierarchyItemData},
        data::{PanelData, PanelSection, default_panel_order},
        kind::PanelKind,
        position::{PanelContainerPosition, PanelPosition},
    },
    proxy::{ProxyData, new_proxy},
    rename::RenameData,
    run::RunProcess,
    source_control::SourceControlData,
    terminal::{
        event::{TermEvent, TermNotification, terminal_update_process},
        panel::TerminalPanelData,
    },
    tracing::*,
    window::WindowCommonData,
    workspace::{PlanckWorkspace, SPACE_EXTENSION, WorkspaceInfo, add_space_folder},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Focus {
    Workbench,
    Palette,
    CodeAction,
    Rename,
    AboutPopup,
    Panel(PanelKind),
}

#[derive(Clone)]
pub enum DragContent {
    Panel(PanelKind),
    EditorTab(EditorTabChild),
    File(PathBuf),
}

impl DragContent {
    pub fn is_panel(&self) -> bool {
        matches!(self, DragContent::Panel(_))
    }
}

#[derive(Clone)]
pub struct WorkProgress {
    pub token: ProgressToken,
    pub title: String,
    pub message: Option<String>,
    pub percentage: Option<u32>,
}

#[derive(Clone)]
pub struct CommonData {
    pub workspace: Arc<PlanckWorkspace>,
    pub scope: Scope,
    pub focus: RwSignal<Focus>,
    pub keypress: RwSignal<KeyPressData>,
    pub completion: RwSignal<CompletionData>,
    pub inline_completion: RwSignal<InlineCompletionData>,
    pub hover: HoverData,
    pub register: RwSignal<Register>,
    pub find: Find,
    pub workbench_size: RwSignal<Size>,
    pub window_origin: RwSignal<Point>,
    pub internal_command: Listener<InternalCommand>,
    pub planck_command: Listener<PlanckCommand>,
    pub workbench_command: Listener<PlanckWorkbenchCommand>,
    pub term_tx: Sender<(TermId, TermEvent)>,
    pub term_notification_tx: Sender<TermNotification>,
    pub proxy: ProxyRpcHandler,
    pub view_id: RwSignal<ViewId>,
    pub ui_line_height: Memo<f64>,
    pub dragging: RwSignal<Option<DragContent>>,
    pub config: ReadSignal<Arc<PlanckConfig>>,
    pub proxy_status: RwSignal<Option<ProxyStatus>>,
    pub mouse_hover_timer: RwSignal<TimerToken>,
    // the current focused view which will receive keyboard events
    pub keyboard_focus: RwSignal<Option<ViewId>>,
    pub window_common: Rc<WindowCommonData>,
}

impl std::fmt::Debug for CommonData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommonData")
            .field("workspace", &self.workspace)
            .finish()
    }
}

#[derive(Clone)]
pub struct WindowTabData {
    pub scope: Scope,
    pub window_tab_id: WindowTabId,
    pub workspace: Arc<PlanckWorkspace>,
    pub palette: PaletteData,
    pub main_split: MainSplitData,
    pub file_explorer: FileExplorerData,
    pub panel: PanelData,
    pub terminal: TerminalPanelData,
    pub code_action: RwSignal<CodeActionData>,
    pub code_lens: RwSignal<Option<ViewId>>,
    pub source_control: SourceControlData,
    pub rename: RenameData,
    pub global_search: GlobalSearchData,
    pub call_hierarchy_data: CallHierarchyData,
    pub about_data: AboutData,
    pub alert_data: AlertBoxData,
    pub layout_rect: RwSignal<Rect>,
    pub title_height: RwSignal<f64>,
    pub status_height: RwSignal<f64>,
    pub proxy: ProxyData,
    pub set_config: WriteSignal<Arc<PlanckConfig>>,
    pub progresses: RwSignal<IndexMap<ProgressToken, WorkProgress>>,
    pub messages: RwSignal<Vec<(String, ShowMessageParams)>>,
    pub common: Rc<CommonData>,
}

impl std::fmt::Debug for WindowTabData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WindowTabData")
            .field("window_tab_id", &self.window_tab_id)
            .finish()
    }
}

impl KeyPressFocus for WindowTabData {
    fn get_mode(&self) -> Mode {
        Mode::Normal
    }

    fn check_condition(&self, condition: Condition) -> bool {
        match condition {
            Condition::PanelFocus => {
                matches!(self.common.focus.get_untracked(), Focus::Panel(_))
            }
            Condition::SourceControlFocus => {
                self.common.focus.get_untracked()
                    == Focus::Panel(PanelKind::SourceControl)
            }
            Condition::FileExplorerFocus => {
                self.common.focus.get_untracked()
                    == Focus::Panel(PanelKind::FileExplorer)
            }
            _ => false,
        }
    }

    fn run_command(
        &self,
        command: &PlanckCommand,
        _count: Option<usize>,
        _mods: Modifiers,
    ) -> CommandExecuted {
        match &command.kind {
            CommandKind::Workbench(cmd) => {
                self.run_workbench_command(cmd.clone(), None);
            }
            CommandKind::Focus(cmd) => {
                if self.common.focus.get_untracked() == Focus::Workbench {
                    match cmd {
                        FocusCommand::SplitClose => {
                            self.main_split.editor_tab_child_close_active();
                        }
                        FocusCommand::SplitVertical => {
                            self.main_split.split_active(SplitDirection::Vertical);
                        }
                        FocusCommand::SplitHorizontal => {
                            self.main_split.split_active(SplitDirection::Horizontal);
                        }
                        FocusCommand::SplitRight => {
                            self.main_split
                                .split_move_active(SplitMoveDirection::Right);
                        }
                        FocusCommand::SplitLeft => {
                            self.main_split
                                .split_move_active(SplitMoveDirection::Left);
                        }
                        FocusCommand::SplitUp => {
                            self.main_split
                                .split_move_active(SplitMoveDirection::Up);
                        }
                        FocusCommand::SplitDown => {
                            self.main_split
                                .split_move_active(SplitMoveDirection::Down);
                        }
                        FocusCommand::SplitExchange => {
                            self.main_split.split_exchange_active();
                        }
                        _ => {
                            return CommandExecuted::No;
                        }
                    }
                }
            }
            _ => {
                return CommandExecuted::No;
            }
        }

        CommandExecuted::Yes
    }

    fn receive_char(&self, _c: &str) {}
}

const SPACE_FILE_SPEC: FileSpec = FileSpec {
    name: "Planck Space",
    extensions: &[SPACE_EXTENSION],
};

/// Adds folders to a space file, then reopens the space in the current window tab.
fn add_folder_and_reopen(
    window_command: Listener<WindowCommand>,
    space: PathBuf,
    folders: Vec<PathBuf>,
) {
    for folder in &folders {
        if let Err(err) = add_space_folder(&space, folder) {
            tracing::error!("failed to add {folder:?} to {space:?}: {err:?}");
            return;
        }
    }
    window_command.send(WindowCommand::SetWorkspace {
        workspace: PlanckWorkspace::from_path(space),
    });
}

impl WindowTabData {
    pub fn new(
        cx: Scope,
        workspace: Arc<PlanckWorkspace>,
        window_common: Rc<WindowCommonData>,
    ) -> Self {
        Self::with_tabs(cx, workspace, window_common, CarriedTabs::default())
    }

    /// A window tab that also opens `carried`, the tabs of the one it replaces.
    pub fn with_tabs(
        cx: Scope,
        workspace: Arc<PlanckWorkspace>,
        window_common: Rc<WindowCommonData>,
        carried: CarriedTabs,
    ) -> Self {
        let cx = cx.create_child();
        let db: Arc<PlanckDb> = use_context().unwrap();
        // Re-read the space file on every open so edits made outside Planck apply.
        let workspace = if workspace.space.is_some() {
            Arc::new((*workspace).clone().load_space())
        } else {
            workspace
        };

        // The window has one tab per workspace, so this tab alone restores and
        // saves the workspace's session.
        let workspace_info = db.get_workspace_info(&workspace).ok();

        let config = PlanckConfig::load(&workspace);
        let planck_command = Listener::new_empty(cx);
        let workbench_command = Listener::new_empty(cx);
        let internal_command = Listener::new_empty(cx);
        let keypress = cx.create_rw_signal(KeyPressData::new(cx, &config));
        let proxy_status = cx.create_rw_signal(None);

        let (term_tx, term_rx) = channel();
        let (term_notification_tx, term_notification_rx) = channel();
        {
            let term_notification_tx = term_notification_tx.clone();
            std::thread::Builder::new()
                .name("terminal update process".to_owned())
                .spawn(move || {
                    terminal_update_process(term_rx, term_notification_tx);
                })
                .unwrap();
        }

        let proxy =
            new_proxy(workspace.clone(), config.lsp.clone(), term_tx.clone());
        let (config, set_config) = cx.create_signal(Arc::new(config));

        let focus = cx.create_rw_signal(Focus::Workbench);
        let completion = cx.create_rw_signal(CompletionData::new(cx, config));
        let inline_completion = cx.create_rw_signal(InlineCompletionData::new(cx));
        let hover = HoverData::new(cx);

        let register = cx.create_rw_signal(Register::default());
        let view_id = cx.create_rw_signal(ViewId::new());
        let find = Find::new(cx);

        let ui_line_height = cx.create_memo(move |_| {
            let config = config.get();
            let mut text_layout = TextLayout::new();

            let family: Vec<FamilyOwned> =
                FamilyOwned::parse_list(&config.ui.font_family).collect();
            let attrs = Attrs::new()
                .family(&family)
                .font_size(config.ui.font_size() as f32)
                .line_height(LineHeightValue::Normal(1.8));
            let attrs_list = AttrsList::new(attrs);
            text_layout.set_text("W", attrs_list, None);
            text_layout.size().height
        });

        let common = Rc::new(CommonData {
            workspace: workspace.clone(),
            scope: cx,
            keypress,
            focus,
            completion,
            inline_completion,
            hover,
            register,
            find,
            internal_command,
            planck_command,
            workbench_command,
            term_tx,
            term_notification_tx,
            proxy: proxy.proxy_rpc.clone(),
            view_id,
            ui_line_height,
            dragging: cx.create_rw_signal(None),
            workbench_size: cx.create_rw_signal(Size::ZERO),
            config,
            proxy_status,
            mouse_hover_timer: cx.create_rw_signal(TimerToken::INVALID),
            window_origin: cx.create_rw_signal(Point::ZERO),
            keyboard_focus: cx.create_rw_signal(None),
            window_common: window_common.clone(),
        });

        let main_split = MainSplitData::new(cx, common.clone());
        let code_action =
            cx.create_rw_signal(CodeActionData::new(cx, common.clone()));
        let source_control =
            SourceControlData::new(cx, main_split.editors, common.clone());
        let file_explorer = FileExplorerData::new(
            cx,
            main_split.editors,
            common.clone(),
            workspace_info
                .as_ref()
                .and_then(|info| info.explorer.as_ref()),
        );

        let mut split = workspace_info.as_ref().map(|info| info.split.clone());
        if !carried.editors.is_empty() {
            split
                .get_or_insert_with(|| SplitInfo {
                    children: Vec::new(),
                    direction: SplitDirection::Horizontal,
                })
                .add_carried(carried);
        }
        if let Some(split) = split {
            let root_split = main_split.root_split;
            split.to_data(main_split.clone(), None, root_split);
        } else {
            let root_split = main_split.root_split;
            let root_split_data = {
                let cx = cx.create_child();
                let root_split_data = SplitData {
                    scope: cx,
                    parent_split: None,
                    split_id: root_split,
                    children: Vec::new(),
                    direction: SplitDirection::Horizontal,
                    window_origin: Point::ZERO,
                    layout_rect: Rect::ZERO,
                };
                cx.create_rw_signal(root_split_data)
            };
            main_split.splits.update(|splits| {
                splits.insert(root_split, root_split_data);
            });
        }

        let palette = PaletteData::new(
            cx,
            workspace.clone(),
            main_split.clone(),
            keypress.read_only(),
            source_control.clone(),
            common.clone(),
        );

        let title_height = cx.create_rw_signal(0.0);
        let status_height = cx.create_rw_signal(0.0);
        let panel_available_size = cx.create_memo(move |_| {
            let title_height = title_height.get();
            let status_height = status_height.get();
            let num_window_tabs = window_common.num_window_tabs.get();
            let window_size = window_common.size.get();
            Size::new(
                window_size.width,
                window_size.height
                    - title_height
                    - status_height
                    - if num_window_tabs > 1 {
                        window_common.window_tab_header_height.get()
                    } else {
                        0.0
                    },
            )
        });
        let panel = workspace_info
            .as_ref()
            .map(|i| {
                let panel_order = db
                    .get_panel_orders()
                    .unwrap_or_else(|_| default_panel_order());
                PanelData {
                    panels: cx.create_rw_signal(panel_order),
                    styles: cx.create_rw_signal(i.panel.styles.clone()),
                    size: cx.create_rw_signal(i.panel.size.clone()),
                    available_size: panel_available_size,
                    sections: cx.create_rw_signal(
                        i.panel
                            .sections
                            .iter()
                            .map(|(key, value)| (*key, cx.create_rw_signal(*value)))
                            .collect(),
                    ),
                    common: common.clone(),
                }
            })
            .unwrap_or_else(|| {
                let panel_order = db
                    .get_panel_orders()
                    .unwrap_or_else(|_| default_panel_order());
                PanelData::new(
                    cx,
                    panel_order,
                    panel_available_size,
                    im::HashMap::new(),
                    common.clone(),
                )
            });
        // A new files-only session starts with the left bar and the bottom panel
        // hidden (the status bar buttons bring them back); a saved one restores them.
        if workspace.is_files_only() && workspace_info.is_none() {
            for position in [
                PanelPosition::LeftTop,
                PanelPosition::LeftBottom,
                PanelPosition::BottomLeft,
                PanelPosition::BottomRight,
            ] {
                panel.set_shown(&position, false);
            }
        }

        let terminal = TerminalPanelData::new(
            workspace.clone(),
            common.config.get_untracked().terminal.get_default_profile(),
            common.clone(),
            main_split.clone(),
        );

        let rename = RenameData::new(cx, main_split.editors, common.clone());
        let global_search = GlobalSearchData::new(cx, main_split.clone());

        {
            let notification = create_signal_from_channel(term_notification_rx);
            let terminal = terminal.clone();
            cx.create_effect(move |_| {
                notification.with(|notification| {
                    if let Some(notification) = notification.as_ref() {
                        match notification {
                            TermNotification::SetTitle { term_id, title } => {
                                terminal.set_title(term_id, title);
                            }
                            TermNotification::RequestPaint => {
                                view_id.get_untracked().request_paint();
                            }
                        }
                    }
                });
            });
        }

        let about_data = AboutData::new(cx, common.focus);
        let alert_data = AlertBoxData::new(cx, common.clone());

        let window_tab_data = Self {
            scope: cx,
            window_tab_id: WindowTabId::next(),
            workspace,
            palette,
            main_split,
            terminal,
            panel,
            file_explorer,
            code_action,
            code_lens: cx.create_rw_signal(None),
            source_control,
            rename,
            global_search,
            call_hierarchy_data: CallHierarchyData {
                root: cx.create_rw_signal(None),
                common: common.clone(),
                scroll_to_line: cx.create_rw_signal(None),
            },
            about_data,
            alert_data,
            layout_rect: cx.create_rw_signal(Rect::ZERO),
            title_height,
            status_height,
            proxy,
            set_config,
            progresses: cx.create_rw_signal(IndexMap::new()),
            messages: cx.create_rw_signal(Vec::new()),
            common,
        };

        {
            let focus = window_tab_data.common.focus;
            let active_editor = window_tab_data.main_split.active_editor;
            let rename_active = window_tab_data.rename.active;
            let internal_command = window_tab_data.common.internal_command;
            cx.create_effect(move |_| {
                let focus = focus.get();
                active_editor.track();
                internal_command.send(InternalCommand::ResetBlinkCursor);

                if focus != Focus::Rename && rename_active.get_untracked() {
                    rename_active.set(false);
                }
            });
        }

        {
            let window_tab_data = window_tab_data.clone();
            window_tab_data.common.planck_command.listen(move |cmd| {
                window_tab_data.run_planck_command(cmd);
            });
        }

        {
            let window_tab_data = window_tab_data.clone();
            window_tab_data.common.workbench_command.listen(move |cmd| {
                window_tab_data.run_workbench_command(cmd, None);
            });
        }

        {
            let window_tab_data = window_tab_data.clone();
            let internal_command = window_tab_data.common.internal_command;
            internal_command.listen(move |cmd| {
                window_tab_data.run_internal_command(cmd);
            });
        }

        {
            let window_tab_data = window_tab_data.clone();
            let notification = window_tab_data.proxy.notification;
            cx.create_effect(move |_| {
                notification.with(|rpc| {
                    if let Some(rpc) = rpc.as_ref() {
                        window_tab_data.handle_core_notification(rpc);
                    }
                });
            });
        }

        if let Some(space) = window_tab_data.workspace.space.clone() {
            window_tab_data.watch_space_settings(cx, &space);
        }

        window_tab_data
    }

    /// Reloads the config when the space file changes. The watcher lives in the
    /// effect, so it stops when this window tab's scope is disposed.
    fn watch_space_settings(&self, cx: Scope, space: &Path) {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut watcher = match notify::recommended_watcher(ConfigWatcher::new(tx)) {
            Ok(watcher) => watcher,
            Err(err) => {
                tracing::error!("{:?}", err);
                return;
            }
        };
        if let Err(err) = watcher.watch(space, notify::RecursiveMode::NonRecursive) {
            tracing::error!("{:?}", err);
            return;
        }
        let changed = create_signal_from_channel(rx);
        let window_tab_data = self.clone();
        cx.create_effect(move |_| {
            let _watcher = &watcher;
            if changed.get().is_some() {
                window_tab_data.reload_config();
            }
        });
    }

    pub fn reload_config(&self) {
        let config = PlanckConfig::load(&self.workspace);
        self.common.keypress.update(|keypress| {
            keypress.update_keymaps(&config);
        });

        let lsp_changed = self.common.config.get_untracked().lsp != config.lsp;
        self.set_config.set(Arc::new(config.clone()));
        if lsp_changed {
            self.common.proxy.update_lsp_servers(config.lsp);
        }
    }

    pub fn run_planck_command(&self, cmd: PlanckCommand) {
        match cmd.kind {
            CommandKind::Workbench(command) => {
                self.run_workbench_command(command, cmd.data);
            }
            CommandKind::Scroll(_)
            | CommandKind::Focus(_)
            | CommandKind::Edit(_)
            | CommandKind::Move(_) => {
                if self.palette.status.get_untracked() != PaletteStatus::Inactive {
                    self.palette.run_command(&cmd, None, Modifiers::empty());
                } else if let Some(editor_data) =
                    self.main_split.active_editor.get_untracked()
                {
                    editor_data.run_command(&cmd, None, Modifiers::empty());
                } else {
                    // TODO: dispatch to current focused view?
                }
            }
            CommandKind::MotionMode(_) => {}
            CommandKind::MultiSelection(_) => {}
        }
    }

    pub fn run_workbench_command(
        &self,
        cmd: PlanckWorkbenchCommand,
        data: Option<Value>,
    ) {
        use PlanckWorkbenchCommand::*;
        match cmd {
            // ==== Modal ====
            EnableModal => {
                let internal_command = self.common.internal_command;
                internal_command.send(InternalCommand::SetModal { modal: true });
            }
            DisableModal => {
                let internal_command = self.common.internal_command;
                internal_command.send(InternalCommand::SetModal { modal: false });
            }

            // ==== Files / Folders ====
            OpenFolder => {
                let window_command = self.common.window_common.window_command;
                let mut options = FileDialogOptions::new()
                    .title("Choose a folder")
                    .select_directories();
                options = if let Some(parent) =
                    self.workspace.path.as_ref().and_then(|x| x.parent())
                {
                    options.force_starting_directory(parent)
                } else {
                    options
                };
                open_file(options, move |file| {
                    if let Some(mut file) = file {
                        let Some(path) = file.path.pop() else {
                            tracing::error!("No path");
                            return;
                        };
                        let workspace = PlanckWorkspace::from_path(path);
                        window_command
                            .send(WindowCommand::SetWorkspace { workspace });
                    }
                });
            }
            OpenSpace => {
                let window_command = self.common.window_common.window_command;
                let options = FileDialogOptions::new()
                    .title("Choose a space")
                    .allowed_types(vec![SPACE_FILE_SPEC]);
                open_file(options, move |file| {
                    if let Some(path) = file.and_then(|mut f| f.path.pop()) {
                        window_command.send(WindowCommand::SetWorkspace {
                            workspace: PlanckWorkspace::from_path(path),
                        });
                    }
                });
            }
            AddFolderToSpace => {
                let window_command = self.common.window_common.window_command;
                let current = (*self.workspace).clone();
                let mut options = FileDialogOptions::new()
                    .title("Choose a folder to add")
                    .select_directories();
                if let Some(parent) = current.path.as_ref().and_then(|p| p.parent())
                {
                    options = options.force_starting_directory(parent);
                }
                open_file(options, move |file| {
                    let Some(folder) = file.and_then(|mut f| f.path.pop()) else {
                        return;
                    };
                    if let Some(space) = current.space.clone() {
                        add_folder_and_reopen(window_command, space, vec![folder]);
                        return;
                    }
                    // No space yet: save one holding the open folder plus the new one.
                    let folders: Vec<PathBuf> =
                        current.path.iter().cloned().chain([folder]).collect();
                    let mut options = FileDialogOptions::new()
                        .title("Save the new space")
                        .allowed_types(vec![SPACE_FILE_SPEC]);
                    if let Some(first) = folders.first() {
                        let name =
                            first.file_name().unwrap_or_default().to_string_lossy();
                        options = options
                            .default_name(format!("{name}.{SPACE_EXTENSION}"));
                        if let Some(parent) = first.parent() {
                            options = options.force_starting_directory(parent);
                        }
                    }
                    save_as(options, move |file| {
                        if let Some(mut space) = file.and_then(|mut f| f.path.pop())
                        {
                            if !PlanckWorkspace::is_space_file(&space) {
                                space.set_extension(SPACE_EXTENSION);
                            }
                            add_folder_and_reopen(
                                window_command,
                                space,
                                folders.clone(),
                            );
                        }
                    });
                });
            }
            CloseFolder => {
                if !self.workspace.is_files_only() {
                    let window_command = self.common.window_common.window_command;
                    let workspace = PlanckWorkspace::default();
                    window_command.send(WindowCommand::SetWorkspace { workspace });
                }
            }
            OpenFile => {
                let internal_command = self.common.internal_command;
                let options = FileDialogOptions::new().title("Choose a file");
                open_file(options, move |file| {
                    if let Some(mut file) = file {
                        internal_command.send(InternalCommand::OpenFile {
                            path: if let Some(path) = file.path.pop() {
                                path
                            } else {
                                tracing::error!("No path");
                                return;
                            },
                        })
                    }
                });
            }
            NewFile => {
                self.main_split.new_file();
            }
            RevealActiveFileInFileExplorer => {
                if let Some(editor_data) = self.main_split.active_editor.get() {
                    let doc = editor_data.doc();
                    let path = if let DocContent::File { path, .. } =
                        doc.content.get_untracked()
                    {
                        Some(path)
                    } else {
                        None
                    };
                    let Some(path) = path else { return };
                    let path = path.parent().unwrap_or(&path);

                    open_uri(path);
                }
            }

            SaveAll => {
                self.main_split.editors.with_editors_untracked(|editors| {
                    let mut paths = HashSet::new();
                    for (editor_id, editor_data) in editors.iter() {
                        // Defensive safeguard: `floem`'s `Editor::doc()` panics if the reactive scope was
                        // already destroyed. This catches "ghost" editors (like closed Settings or Plugin tabs)
                        // that leaked into the map, preventing "Save All" from crashing the entire app.
                        let doc_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            editor_data.doc()
                        }));

                        let doc = match doc_result {
                            Ok(document) => document,
                            Err(_) => {
                                tracing::warn!(
                                    "Ghost editor caught and bypassed during Save All. View ID: {:?}",
                                    editor_id
                                );
                                continue;
                            }
                        };

                        let should_save = if let DocContent::File { path, .. } =
                            doc.content.get_untracked()
                        {
                            if paths.contains(&path) {
                                false
                            } else {
                                paths.insert(path.clone());
                                true
                            }
                        } else {
                            false
                        };

                        if should_save {
                            editor_data.save(true, || {});
                        }
                    }
                });
            }

            // ==== Configuration / Info Files and Folders ====
            OpenSettings => {
                self.main_split.open_settings();
            }
            OpenSettingsFile => {
                if let Some(path) = PlanckConfig::settings_file() {
                    self.main_split.jump_to_location(
                        EditorLocation {
                            path,
                            position: None,
                            scroll_offset: None,
                            ignore_unconfirmed: false,
                            same_editor_tab: false,
                        },
                        None,
                    );
                }
            }
            OpenSettingsDirectory => {
                if let Some(dir) = Directory::config_directory() {
                    open_uri(&dir);
                }
            }
            OpenThemeColorSettings => {
                self.main_split.open_theme_color_settings();
            }
            OpenKeyboardShortcuts => {
                self.main_split.open_keymap();
            }
            OpenKeyboardShortcutsFile => {
                if let Some(path) = PlanckConfig::keymaps_file() {
                    self.main_split.jump_to_location(
                        EditorLocation {
                            path,
                            position: None,
                            scroll_offset: None,
                            ignore_unconfirmed: false,
                            same_editor_tab: false,
                        },
                        None,
                    );
                }
            }
            OpenLogFile => {
                if let Some(dir) = Directory::logs_directory() {
                    self.open_paths(&[PathObject::from_path(
                        dir.join(format!(
                            "planck.{}.log",
                            chrono::prelude::Local::now().format("%Y-%m-%d")
                        )),
                        false,
                    )])
                }
            }
            OpenLogsDirectory => {
                if let Some(dir) = Directory::logs_directory() {
                    open_uri(&dir);
                }
            }
            OpenProxyDirectory => {
                if let Some(dir) = Directory::proxy_directory() {
                    open_uri(&dir);
                }
            }
            OpenThemesDirectory => {
                if let Some(dir) = Directory::themes_directory() {
                    open_uri(&dir);
                }
            }
            OpenGrammarsDirectory => {
                if let Some(dir) = Directory::grammars_directory() {
                    open_uri(&dir);
                }
            }
            OpenQueriesDirectory => {
                if let Some(dir) = Directory::queries_directory() {
                    open_uri(&dir);
                }
            }

            InstallTheme => {}
            ExportCurrentThemeSettings => {
                self.main_split.export_theme();
            }
            ToggleInlayHints => {}
            ToggleMarkdownRichView => {
                if let Some(editor) = self.main_split.active_editor.get_untracked() {
                    editor.markdown_rich.update(|rich| *rich = !*rich);
                }
            }
            ToggleMinimap => {
                let enabled = self.common.config.get_untracked().editor.minimap;
                PlanckConfig::update_file(
                    None,
                    "editor",
                    "minimap",
                    toml_edit::Value::from(!enabled),
                );
            }
            ToggleLineBlame => {
                let enabled =
                    self.common.config.get_untracked().editor.current_line_blame;
                PlanckConfig::update_file(
                    None,
                    "editor",
                    "current-line-blame",
                    toml_edit::Value::from(!enabled),
                );
            }

            // ==== Window ====
            ReloadWindow => {
                self.common.window_common.window_command.send(
                    WindowCommand::SetWorkspace {
                        workspace: (*self.workspace).clone(),
                    },
                );
            }
            CloseWindow => {
                self.common
                    .window_common
                    .window_command
                    .send(WindowCommand::CloseWindow);
            }
            // ==== Window Tabs ====
            NewWindowTab => {
                self.common.window_common.window_command.send(
                    WindowCommand::NewWorkspaceTab {
                        workspace: PlanckWorkspace::default(),
                        end: false,
                    },
                );
            }
            CloseWindowTab => {
                self.common
                    .window_common
                    .window_command
                    .send(WindowCommand::CloseWorkspaceTab { index: None });
            }
            NextWindowTab => {
                self.common
                    .window_common
                    .window_command
                    .send(WindowCommand::NextWorkspaceTab);
            }
            PreviousWindowTab => {
                self.common
                    .window_common
                    .window_command
                    .send(WindowCommand::PreviousWorkspaceTab);
            }

            // ==== Editor Tabs ====
            NextEditorTab => {
                if let Some(editor_tab_id) =
                    self.main_split.active_editor_tab.get_untracked()
                {
                    self.main_split.editor_tabs.with_untracked(|editor_tabs| {
                        let Some(editor_tab) = editor_tabs.get(&editor_tab_id)
                        else {
                            return;
                        };

                        let new_index = editor_tab.with_untracked(|editor_tab| {
                            if editor_tab.children.is_empty() {
                                None
                            } else if editor_tab.active
                                == editor_tab.children.len() - 1
                            {
                                Some(0)
                            } else {
                                Some(editor_tab.active + 1)
                            }
                        });

                        if let Some(new_index) = new_index {
                            editor_tab.update(|editor_tab| {
                                editor_tab.active = new_index;
                            });
                        }
                    });
                }
            }
            PreviousEditorTab => {
                if let Some(editor_tab_id) =
                    self.main_split.active_editor_tab.get_untracked()
                {
                    self.main_split.editor_tabs.with_untracked(|editor_tabs| {
                        let Some(editor_tab) = editor_tabs.get(&editor_tab_id)
                        else {
                            return;
                        };

                        let new_index = editor_tab.with_untracked(|editor_tab| {
                            if editor_tab.children.is_empty() {
                                None
                            } else if editor_tab.active == 0 {
                                Some(editor_tab.children.len() - 1)
                            } else {
                                Some(editor_tab.active - 1)
                            }
                        });

                        if let Some(new_index) = new_index {
                            editor_tab.update(|editor_tab| {
                                editor_tab.active = new_index;
                            });
                        }
                    });
                }
            }

            // ==== File Explorer ====
            FileExplorerRename => {
                self.file_explorer.rename_selected();
            }
            FileExplorerTrash => {
                self.file_explorer.trash_selected();
            }

            // ==== Terminal ====
            NewTerminalTab => {
                self.terminal.new_tab(
                    self.common
                        .config
                        .get_untracked()
                        .terminal
                        .get_default_profile(),
                );
                if !self.panel.is_panel_visible(&PanelKind::Terminal) {
                    self.panel.show_panel(&PanelKind::Terminal);
                }
                self.common.focus.set(Focus::Panel(PanelKind::Terminal));
            }
            CloseTerminalTab => {
                self.terminal.close_tab(None);
                if self
                    .terminal
                    .tab_info
                    .with_untracked(|info| info.tabs.is_empty())
                {
                    if self.panel.is_panel_visible(&PanelKind::Terminal) {
                        self.panel.hide_panel(&PanelKind::Terminal);
                    }
                    self.common.focus.set(Focus::Workbench);
                } else {
                    if !self.panel.is_panel_visible(&PanelKind::Terminal) {
                        self.panel.show_panel(&PanelKind::Terminal);
                    }
                    self.common.focus.set(Focus::Panel(PanelKind::Terminal));
                }
            }
            NextTerminalTab => {
                self.terminal.next_tab();
                if !self.panel.is_panel_visible(&PanelKind::Terminal) {
                    self.panel.show_panel(&PanelKind::Terminal);
                }
                self.common.focus.set(Focus::Panel(PanelKind::Terminal));
            }
            PreviousTerminalTab => {
                self.terminal.previous_tab();
                if !self.panel.is_panel_visible(&PanelKind::Terminal) {
                    self.panel.show_panel(&PanelKind::Terminal);
                }
                self.common.focus.set(Focus::Panel(PanelKind::Terminal));
            }

            // ==== Palette Commands ====
            PaletteHelp => self.palette.run(PaletteKind::PaletteHelp),
            PaletteHelpAndFile => self.palette.run(PaletteKind::HelpAndFile),
            PaletteLine => {
                self.palette.run(PaletteKind::Line);
            }
            Palette => {
                self.palette.run(PaletteKind::File);
            }
            PaletteSymbol => {
                self.palette.run(PaletteKind::DocumentSymbol);
            }
            PaletteWorkspaceSymbol => {}
            PaletteCommand => {
                self.palette.run(PaletteKind::Command);
            }
            PaletteWorkspace => {
                self.palette.run(PaletteKind::Workspace);
            }
            PaletteRun => {
                self.palette.run(PaletteKind::Run);
            }
            PaletteCompareRevision => {
                self.palette.run(PaletteKind::CompareRevision);
            }
            PaletteSCMReferences => {
                self.palette.run(PaletteKind::SCMReferences);
            }
            ChangeColorTheme => {
                self.palette.run(PaletteKind::ColorTheme);
            }
            ChangeIconTheme => {
                self.palette.run(PaletteKind::IconTheme);
            }
            ChangeFileLanguage => {
                self.palette.run(PaletteKind::Language);
            }
            ChangeFileLineEnding => {
                self.palette.run(PaletteKind::LineEnding);
            }
            DiffFiles => self.palette.run(PaletteKind::DiffFiles),

            // ==== Running ====
            RunRestart => {
                if self
                    .active_run_term()
                    .and_then(|term_id| self.terminal.restart_run(term_id))
                    .is_some()
                {
                    self.panel.show_panel(&PanelKind::Terminal);
                } else {
                    self.palette.run(PaletteKind::Run);
                }
            }
            RunStop => {
                if let Some(term_id) = self.active_run_term() {
                    self.terminal.stop_run(term_id);
                }
            }

            // ==== UI ====
            ZoomIn => {
                let mut scale =
                    self.common.window_common.window_scale.get_untracked();
                scale += 0.1;
                if scale > 4.0 {
                    scale = 4.0
                }
                self.common.window_common.window_scale.set(scale);

                PlanckConfig::update_file(
                    None,
                    "ui",
                    "scale",
                    toml_edit::Value::from(scale),
                );
            }
            ZoomOut => {
                let mut scale =
                    self.common.window_common.window_scale.get_untracked();
                scale -= 0.1;
                if scale < 0.1 {
                    scale = 0.1
                }
                self.common.window_common.window_scale.set(scale);

                PlanckConfig::update_file(
                    None,
                    "ui",
                    "scale",
                    toml_edit::Value::from(scale),
                );
            }
            ZoomReset => {
                self.common.window_common.window_scale.set(1.0);

                PlanckConfig::update_file(
                    None,
                    "ui",
                    "scale",
                    toml_edit::Value::from(1.0),
                );
            }

            ToggleMaximizedPanel => {
                if let Some(data) = data {
                    if let Ok(kind) = serde_json::from_value::<PanelKind>(data) {
                        self.panel.toggle_maximize(&kind);
                    }
                } else {
                    self.panel.toggle_active_maximize();
                }
            }
            HidePanel => {
                if let Some(data) = data {
                    if let Ok(kind) = serde_json::from_value::<PanelKind>(data) {
                        self.hide_panel(kind);
                    }
                }
            }
            ShowPanel => {
                if let Some(data) = data {
                    if let Ok(kind) = serde_json::from_value::<PanelKind>(data) {
                        self.show_panel(kind);
                    }
                }
            }
            TogglePanelFocus => {
                if let Some(data) = data {
                    if let Ok(kind) = serde_json::from_value::<PanelKind>(data) {
                        self.toggle_panel_focus(kind);
                    }
                }
            }
            TogglePanelVisual => {
                if let Some(data) = data {
                    if let Ok(kind) = serde_json::from_value::<PanelKind>(data) {
                        self.toggle_panel_visual(kind);
                    }
                }
            }
            TogglePanelLeftVisual => {
                self.toggle_container_visual(&PanelContainerPosition::Left);
            }
            TogglePanelRightVisual => {
                self.toggle_container_visual(&PanelContainerPosition::Right);
            }
            TogglePanelBottomVisual => {
                self.toggle_container_visual(&PanelContainerPosition::Bottom);
            }
            ToggleTerminalFocus => {
                self.toggle_panel_focus(PanelKind::Terminal);
            }
            ToggleSourceControlFocus => {
                self.toggle_panel_focus(PanelKind::SourceControl);
            }
            ToggleFileExplorerFocus => {
                self.toggle_panel_focus(PanelKind::FileExplorer);
            }
            ToggleProblemFocus => {
                self.toggle_panel_focus(PanelKind::Problem);
            }
            ToggleSearchFocus => {
                self.toggle_panel_focus(PanelKind::Search);
            }
            ToggleTerminalVisual => {
                self.toggle_panel_visual(PanelKind::Terminal);
            }
            ToggleSourceControlVisual => {
                self.toggle_panel_visual(PanelKind::SourceControl);
            }
            ToggleFileExplorerVisual => {
                self.toggle_panel_visual(PanelKind::FileExplorer);
            }
            ToggleProblemVisual => {
                self.toggle_panel_visual(PanelKind::Problem);
            }
            ToggleSearchVisual => {
                self.toggle_panel_visual(PanelKind::Search);
            }
            FocusEditor => {
                self.common.focus.set(Focus::Workbench);
            }
            FocusTerminal => {
                self.common.focus.set(Focus::Panel(PanelKind::Terminal));
            }
            OpenUIInspector => {
                self.common.view_id.get_untracked().inspect();
            }
            ShowEnvironment => {
                self.main_split.show_env();
            }

            // ==== Source Control ====
            SourceControlInit => {
                self.proxy.proxy_rpc.git_init();
            }
            CheckoutReference => match data {
                Some(reference) => {
                    if let Some(reference) = reference.as_str() {
                        self.proxy.proxy_rpc.git_checkout(reference.to_string());
                    }
                }
                None => error!("No ref provided"),
            },
            SourceControlCommit => {
                self.source_control.commit();
            }
            SourceControlCopyActiveFileRemoteUrl => {}
            SourceControlDiscardActiveFileChanges => {}
            SourceControlDiscardTargetFileChanges => {
                if let Some(diff) = data
                    .and_then(|data| serde_json::from_value::<FileDiff>(data).ok())
                {
                    match diff {
                        FileDiff::Added(path) => {
                            self.common.proxy.trash_path(path, Box::new(|_| {}));
                        }
                        FileDiff::Modified(path) | FileDiff::Deleted(path) => {
                            self.common.proxy.git_discard_files_changes(vec![path]);
                        }
                        FileDiff::Renamed(old_path, new_path) => {
                            self.common
                                .proxy
                                .git_discard_files_changes(vec![old_path]);
                            self.common.proxy.trash_path(new_path, Box::new(|_| {}));
                        }
                    }
                }
            }
            SourceControlDiscardWorkspaceChanges => {}

            // ==== UI ====
            ShowAbout => {
                self.about_data.open();
            }

            // ==== Movement ====
            #[cfg(target_os = "macos")]
            InstallToPATH => self.shell_command(true),
            #[cfg(target_os = "macos")]
            UninstallFromPATH => self.shell_command(false),
            JumpLocationForward => {
                self.main_split.jump_location_forward(false);
            }
            JumpLocationBackward => {
                self.main_split.jump_location_backward(false);
            }
            JumpLocationForwardLocal => {
                self.main_split.jump_location_forward(true);
            }
            JumpLocationBackwardLocal => {
                self.main_split.jump_location_backward(true);
            }
            NextError => {
                self.main_split.next_error();
            }
            PreviousError => {}
            Quit => {
                floem::quit_app();
            }
            RevealInPanel => {
                if let Some(editor_data) =
                    self.main_split.active_editor.get_untracked()
                {
                    self.show_panel(PanelKind::FileExplorer);
                    self.panel.section_open(PanelSection::FileExplorer).update(
                        |x| {
                            *x = true;
                        },
                    );
                    if let DocContent::File { path, .. } =
                        editor_data.doc().content.get_untracked()
                    {
                        self.file_explorer.reveal_in_file_tree(path);
                    }
                }
            }
            SourceControlOpenActiveFileRemoteUrl => {
                if let Some(editor_data) =
                    self.main_split.active_editor.get_untracked()
                {
                    if let DocContent::File { path, .. } =
                        editor_data.doc().content.get_untracked()
                    {
                        let offset =
                            editor_data.cursor().with_untracked(|c| c.offset());
                        let line = editor_data
                            .doc()
                            .buffer
                            .with_untracked(|buffer| buffer.line_of_offset(offset));
                        self.common.proxy.git_get_remote_file_url(
                            path,
                            create_ext_action(self.scope, move |result| {
                                if let Ok(ProxyResponse::GitGetRemoteFileUrl {
                                    file_url,
                                }) = result
                                {
                                    if let Err(err) =
                                        open::that(format!("{}#L{}", file_url, line))
                                    {
                                        error!(
                                            "Failed to open file in github: {}",
                                            err
                                        );
                                    }
                                }
                            }),
                        );
                    }
                }
            }
            RevealInFileExplorer => {
                if let Some(editor_data) =
                    self.main_split.active_editor.get_untracked()
                {
                    if let DocContent::File { path, .. } =
                        editor_data.doc().content.get_untracked()
                    {
                        let path = path.parent().unwrap_or(&path);
                        if !path.exists() {
                            return;
                        }
                        if let Err(err) = open::that(path) {
                            error!(
                                "Failed to reveal file in system file explorer: {}",
                                err
                            );
                        }
                    }
                }
            }
            ShowCallHierarchy => {
                if let Some(editor_data) =
                    self.main_split.active_editor.get_untracked()
                {
                    editor_data.call_hierarchy(self.clone());
                }
            }
            FindReferences => {
                if let Some(editor_data) =
                    self.main_split.active_editor.get_untracked()
                {
                    editor_data.find_reference(self.clone());
                }
            }
            GoToImplementation => {
                if let Some(editor_data) =
                    self.main_split.active_editor.get_untracked()
                {
                    editor_data.go_to_implementation(self.clone());
                }
            }
            RunInTerminal => {
                if let Some(editor_data) =
                    self.main_split.active_editor.get_untracked()
                {
                    let name = editor_data.word_at_cursor();
                    if !name.is_empty() {
                        let mut args_str = name.split(" ");
                        let program =
                            args_str.next().map(|x| x.to_string()).unwrap();
                        let args: Vec<String> =
                            args_str.map(|x| x.to_string()).collect();
                        let args = if args.is_empty() { None } else { Some(args) };

                        let config = RunConfig {
                            name,
                            program,
                            args,
                            cwd: None,
                            env: None,
                            prelaunch: None,
                            config_source: ConfigSource::RunInTerminal,
                        };
                        self.common
                            .internal_command
                            .send(InternalCommand::Run { config });
                    }
                }
            }
            GoToLocation => {
                if let Some(editor_data) =
                    self.main_split.active_editor.get_untracked()
                {
                    let doc = editor_data.doc();
                    let path = match if doc.loaded() {
                        doc.content.with_untracked(|c| c.path().cloned())
                    } else {
                        None
                    } {
                        Some(path) => path,
                        None => return,
                    };
                    let offset = editor_data.cursor().with_untracked(|c| c.offset());
                    let internal_command = self.common.internal_command;

                    internal_command.send(InternalCommand::MakeConfirmed);
                    internal_command.send(InternalCommand::GoToLocation {
                        location: EditorLocation {
                            path,
                            position: Some(EditorPosition::Offset(offset)),
                            scroll_offset: None,
                            ignore_unconfirmed: false,
                            same_editor_tab: false,
                        },
                    });
                }
            }
            AddRunConfig => {
                if let Some(editor_data) =
                    self.main_split.active_editor.get_untracked()
                {
                    editor_data.receive_char(DEFAULT_RUN_TOML);
                }
            }

            JumpHighlightNext => {
                if let Some(editor_data) =
                    self.main_split.active_editor.get_untracked()
                {
                    editor_data.jump_highlight(InlineFindDirection::Right);
                }
            }
            JumpHighlightPrev => {
                if let Some(editor_data) =
                    self.main_split.active_editor.get_untracked()
                {
                    editor_data.jump_highlight(InlineFindDirection::Left);
                }
            }
        }
    }

    pub fn run_internal_command(&self, cmd: InternalCommand) {
        match cmd {
            InternalCommand::ReloadConfig => {
                self.reload_config();
            }
            InternalCommand::UpdateLogLevel { level } => {
                // TODO: implement logging panel, runtime log level change
                debug!("{level}");
            }
            InternalCommand::MakeConfirmed => {
                if let Some(editor) = self.main_split.active_editor.get_untracked() {
                    editor.confirmed.set(true);
                }
            }
            InternalCommand::OpenFile { path } => {
                self.main_split.jump_to_location(
                    EditorLocation {
                        path,
                        position: None,
                        scroll_offset: None,
                        ignore_unconfirmed: false,
                        same_editor_tab: false,
                    },
                    None,
                );
            }
            InternalCommand::OpenAndConfirmedFile { path } => {
                self.main_split.jump_to_location(
                    EditorLocation {
                        path,
                        position: None,
                        scroll_offset: None,
                        ignore_unconfirmed: false,
                        same_editor_tab: false,
                    },
                    None,
                );
                if let Some(editor) = self.main_split.active_editor.get_untracked() {
                    editor.confirmed.set(true);
                }
            }
            InternalCommand::OpenFileInNewTab { path } => {
                self.main_split.jump_to_location(
                    EditorLocation {
                        path,
                        position: None,
                        scroll_offset: None,
                        ignore_unconfirmed: true,
                        same_editor_tab: false,
                    },
                    None,
                );
            }
            InternalCommand::OpenFileChanges { path, version } => {
                self.main_split.open_file_changes(path, version);
            }
            InternalCommand::ReloadFileExplorer => {
                self.file_explorer.reload();
            }
            InternalCommand::TestPathCreation { new_path } => {
                let naming = self.file_explorer.naming;

                let send = create_ext_action(
                    self.scope,
                    move |response: Result<ProxyResponse, RpcError>| match response {
                        Ok(_) => {
                            naming.update(Naming::set_ok);
                        }
                        Err(err) => {
                            naming.update(|naming| naming.set_err(err.message));
                        }
                    },
                );

                self.common.proxy.test_create_at_path(new_path, send);
            }
            InternalCommand::FinishRenamePath {
                current_path,
                new_path,
            } => {
                let send_current_path = current_path.clone();
                let send_new_path = new_path.clone();
                let file_explorer = self.file_explorer.clone();
                let editors = self.main_split.editors;

                let send = create_ext_action(
                    self.scope,
                    move |response: Result<ProxyResponse, RpcError>| match response {
                        Ok(response) => {
                            // Get the canonicalized new path from the proxy.
                            let new_path =
                                if let ProxyResponse::CreatePathResponse { path } =
                                    response
                                {
                                    path
                                } else {
                                    send_new_path
                                };

                            // If the renamed item is a file, update any editors the file is open
                            // in to use the new path.
                            // If the renamed item is a directory, update any editors in which a
                            // file the renamed directory is an ancestor of is open to use the
                            // file's new path.
                            let renamed_editors_content: Vec<_> = editors
                                .with_editors_untracked(|editors| {
                                    editors
                                        .values()
                                        .map(|editor| editor.doc().content)
                                        .filter(|content| {
                                            content.with_untracked(|content| {
                                                match content {
                                                    DocContent::File {
                                                        path,
                                                        ..
                                                    } => path.starts_with(
                                                        &send_current_path,
                                                    ),
                                                    _ => false,
                                                }
                                            })
                                        })
                                        .collect()
                                });

                            for content in renamed_editors_content {
                                content.update(|content| {
                                    if let DocContent::File { path, .. } = content {
                                        if let Ok(suffix) =
                                            path.strip_prefix(&send_current_path)
                                        {
                                            *path = new_path.join(suffix);
                                        }
                                    }
                                });
                            }

                            file_explorer.reload();
                            file_explorer.naming.set(Naming::None);
                        }
                        Err(err) => {
                            file_explorer
                                .naming
                                .update(|naming| naming.set_err(err.message));
                        }
                    },
                );

                self.file_explorer.naming.update(Naming::set_pending);
                self.common
                    .proxy
                    .rename_path(current_path.clone(), new_path, send);
            }
            InternalCommand::FinishNewNode { is_dir, path } => {
                let file_explorer = self.file_explorer.clone();
                let internal_command = self.common.internal_command;

                let send = create_ext_action(
                    self.scope,
                    move |response: Result<ProxyResponse, RpcError>| {
                        match response {
                            Ok(response) => {
                                file_explorer.reload();
                                file_explorer.naming.set(Naming::None);

                                // Open a new file in the editor
                                if let ProxyResponse::CreatePathResponse { path } =
                                    response
                                {
                                    if !is_dir {
                                        internal_command.send(
                                            InternalCommand::OpenFile { path },
                                        );
                                    }
                                }
                            }
                            Err(err) => {
                                file_explorer
                                    .naming
                                    .update(|naming| naming.set_err(err.message));
                            }
                        }
                    },
                );

                self.file_explorer.naming.update(Naming::set_pending);
                if is_dir {
                    self.common.proxy.create_directory(path, send);
                } else {
                    self.common.proxy.create_file(path, send);
                }
            }
            InternalCommand::FinishDuplicate { source, path } => {
                let file_explorer = self.file_explorer.clone();

                let send = create_ext_action(
                    self.scope,
                    move |response: Result<_, RpcError>| {
                        if let Err(err) = response {
                            file_explorer
                                .naming
                                .update(|naming| naming.set_err(err.message));
                        } else {
                            file_explorer.reload();
                            file_explorer.naming.set(Naming::None);
                        }
                    },
                );

                self.file_explorer.naming.update(Naming::set_pending);
                self.common.proxy.duplicate_path(source, path, send);
            }
            InternalCommand::GoToLocation { location } => {
                self.main_split.go_to_location(location, None);
            }
            InternalCommand::JumpToLocation { location } => {
                self.main_split.jump_to_location(location, None);
            }
            InternalCommand::PaletteReferences { references } => {
                self.palette.references.set(references);
                self.palette.run(PaletteKind::Reference);
            }
            InternalCommand::Split {
                direction,
                editor_tab_id,
            } => {
                self.main_split.split(direction, editor_tab_id);
            }
            InternalCommand::SplitMove {
                direction,
                editor_tab_id,
            } => {
                self.main_split.split_move(direction, editor_tab_id);
            }
            InternalCommand::SplitExchange { editor_tab_id } => {
                self.main_split.split_exchange(editor_tab_id);
            }
            InternalCommand::EditorTabClose { editor_tab_id } => {
                self.main_split.editor_tab_close(editor_tab_id);
            }
            InternalCommand::EditorTabChildClose {
                editor_tab_id,
                child,
            } => {
                self.main_split
                    .editor_tab_child_close(editor_tab_id, child, false);
            }
            InternalCommand::EditorTabCloseByKind {
                editor_tab_id,
                child,
                kind,
            } => {
                self.main_split.editor_tab_child_close_by_kind(
                    editor_tab_id,
                    child,
                    kind,
                );
            }
            InternalCommand::ShowCodeActions {
                offset,
                mouse_click,
                plugin_id,
                code_actions,
            } => {
                let mut code_action = self.code_action.get_untracked();
                code_action.show(plugin_id, code_actions, offset, mouse_click);
                self.code_action.set(code_action);
            }
            InternalCommand::RunCodeAction { plugin_id, action } => {
                self.main_split.run_code_action(plugin_id, action);
            }
            InternalCommand::ApplyWorkspaceEdit { edit } => {
                self.main_split.apply_workspace_edit(&edit);
            }
            InternalCommand::SaveJumpLocation {
                path,
                offset,
                scroll_offset,
            } => {
                self.main_split
                    .save_jump_location(path, offset, scroll_offset);
            }
            InternalCommand::NewTerminal { profile } => {
                self.terminal.new_tab(profile);
                self.panel.show_panel(&PanelKind::Terminal);
                self.common.focus.set(Focus::Panel(PanelKind::Terminal));
            }
            InternalCommand::SplitTerminal { term_id } => {
                self.terminal.split(term_id);
            }
            InternalCommand::SplitTerminalNext { term_id } => {
                self.terminal.split_next(term_id);
            }
            InternalCommand::SplitTerminalPrevious { term_id } => {
                self.terminal.split_previous(term_id);
            }
            InternalCommand::SplitTerminalExchange { term_id } => {
                self.terminal.split_exchange(term_id);
            }
            InternalCommand::Run { config } => {
                self.run_in_terminal(&config);
            }
            InternalCommand::StartRename {
                path,
                placeholder,
                position,
                start,
            } => {
                self.rename.start(path, placeholder, start, position);
            }
            InternalCommand::Search { pattern } => {
                self.main_split.set_find_pattern(pattern);
            }
            InternalCommand::FindEditorReceiveChar { s } => {
                self.main_split.find_editor.receive_char(&s);
            }
            InternalCommand::ReplaceEditorReceiveChar { s } => {
                self.main_split.replace_editor.receive_char(&s);
            }
            InternalCommand::FindEditorCommand {
                command,
                count,
                mods,
            } => {
                self.main_split
                    .find_editor
                    .run_command(&command, count, mods);
            }
            InternalCommand::ReplaceEditorCommand {
                command,
                count,
                mods,
            } => {
                self.main_split
                    .replace_editor
                    .run_command(&command, count, mods);
            }
            InternalCommand::FocusEditorTab { editor_tab_id } => {
                self.main_split.active_editor_tab.set(Some(editor_tab_id));
            }
            InternalCommand::SetColorTheme { name, save } => {
                if save {
                    // The config file is watched
                    PlanckConfig::update_file(
                        None,
                        "core",
                        "color-theme",
                        toml_edit::Value::from(name),
                    );
                } else {
                    let mut new_config = self.common.config.get_untracked();
                    Arc::make_mut(&mut new_config)
                        .set_color_theme(&self.workspace, &name);
                    self.set_config.set(new_config);
                }
            }
            InternalCommand::SetIconTheme { name, save } => {
                if save {
                    // The config file is watched
                    PlanckConfig::update_file(
                        None,
                        "core",
                        "icon-theme",
                        toml_edit::Value::from(name),
                    );
                } else {
                    let mut new_config = self.common.config.get_untracked();
                    Arc::make_mut(&mut new_config)
                        .set_icon_theme(&self.workspace, &name);
                    self.set_config.set(new_config);
                }
            }
            InternalCommand::SetModal { modal } => {
                PlanckConfig::update_file(
                    None,
                    "core",
                    "modal",
                    toml_edit::Value::from(modal),
                );
            }
            InternalCommand::OpenWebUri { uri } => {
                if !uri.is_empty() {
                    match open::that(&uri) {
                        Ok(_) => {
                            trace!(TraceLevel::TRACE, "opened web uri: {uri:?}");
                        }
                        Err(e) => {
                            trace!(
                                TraceLevel::ERROR,
                                "failed to open web uri: {uri:?}, error: {e}"
                            );
                        }
                    }
                }
            }
            InternalCommand::ShowAlert {
                title,
                msg,
                buttons,
            } => {
                self.show_alert(title, msg, buttons);
            }
            InternalCommand::HideAlert => {
                self.alert_data.active.set(false);
            }
            InternalCommand::SaveSession => {
                let db: Arc<PlanckDb> = use_context().unwrap();
                if let Err(err) = db.save_window_tab(self) {
                    tracing::error!("{:?}", err);
                }
            }
            InternalCommand::SaveScratchDoc { doc } => {
                self.main_split.save_scratch_doc(doc);
            }
            InternalCommand::SaveScratchDoc2 { doc } => {
                self.main_split.save_scratch_doc2(doc);
            }
            InternalCommand::UpdateProxyStatus { status } => {
                self.common.proxy_status.set(Some(status));
            }
            InternalCommand::ResetBlinkCursor => {
                // All the editors share the blinking information and logic, so we can just reset
                // one of them.
                if let Some(e_data) = self.main_split.active_editor.get_untracked() {
                    e_data.editor.cursor_info.reset();
                }
            }
            InternalCommand::OpenDiffFiles {
                left_path,
                right_path,
            } => self.main_split.open_diff_files(left_path, right_path),
            InternalCommand::ClearTerminalBuffer {
                view_id,
                tab_index,
                terminal_index,
            } => {
                let Some(tab) = self.terminal.tab_info.with_untracked(|x| {
                    x.tabs.iter().find_map(|(index, data)| {
                        if index.get_untracked() == tab_index {
                            Some(data.clone())
                        } else {
                            None
                        }
                    })
                }) else {
                    error!("could not find terminal tab data: index={tab_index}");
                    return;
                };
                let Some(raw) = tab.terminals.with_untracked(|x| {
                    x.iter().find_map(|(index, data)| {
                        if index.get_untracked() == terminal_index {
                            Some(data.raw.get_untracked())
                        } else {
                            None
                        }
                    })
                }) else {
                    error!("could not find terminal data: index={terminal_index}");
                    return;
                };
                raw.write().term.reset_state();
                view_id.request_paint();
            }
            InternalCommand::StopTerminal { term_id } => {
                self.terminal.stop_run(term_id);
            }
            InternalCommand::RestartTerminal { term_id } => {
                if self.terminal.restart_run(term_id).is_some() {
                    self.panel.show_panel(&PanelKind::Terminal);
                } else {
                    self.palette.run(PaletteKind::Run);
                }
            }
            InternalCommand::CallHierarchyIncoming { item_id } => {
                self.call_hierarchy_incoming(item_id);
            }
        }
    }

    fn handle_core_notification(&self, rpc: &CoreNotification) {
        match rpc {
            CoreNotification::ProxyStatus { status } => {
                self.common.proxy_status.set(Some(status.to_owned()));
            }
            CoreNotification::DiffInfo { diff } => {
                self.source_control.branch.set(diff.head.clone());
                self.source_control
                    .branches
                    .set(diff.branches.iter().cloned().collect());
                self.source_control
                    .tags
                    .set(diff.tags.iter().cloned().collect());
                self.source_control.file_diffs.update(|file_diffs| {
                    *file_diffs = diff
                        .diffs
                        .iter()
                        .cloned()
                        .map(|diff| {
                            let checked =
                                file_diffs.get(diff.path()).is_none_or(|(_, c)| *c);
                            (diff.path().clone(), (diff, checked))
                        })
                        .collect();
                });

                let docs = self.main_split.docs.get_untracked();
                for (_, doc) in docs {
                    doc.retrieve_head();
                    doc.refresh_blame();
                }
            }
            CoreNotification::CompletionResponse {
                request_id,
                input,
                resp,
                plugin_id,
            } => {
                self.common.completion.update(|completion| {
                    completion.receive(*request_id, input, resp, *plugin_id);
                });

                let completion = self.common.completion.get_untracked();
                let editor_data = completion
                    .latest_editor_id
                    .and_then(|id| self.main_split.editors.editor_untracked(id));
                if let Some(editor_data) = editor_data {
                    let cursor_offset =
                        editor_data.cursor().with_untracked(|c| c.offset());
                    completion
                        .update_document_completion(&editor_data, cursor_offset);
                }
            }
            CoreNotification::PublishDiagnostics { diagnostics } => {
                let path = path_from_url(&diagnostics.uri);
                let diagnostics: im::Vector<Diagnostic> = diagnostics
                    .diagnostics
                    .clone()
                    .into_iter()
                    .sorted_by_key(|d| d.range.start)
                    .collect();

                self.main_split
                    .get_diagnostic_data(&path)
                    .diagnostics
                    .set(diagnostics);

                // inform the document about the diagnostics
                if let Some(doc) = self
                    .main_split
                    .docs
                    .with_untracked(|docs| docs.get(&path).cloned())
                {
                    doc.init_diagnostics();
                }
            }
            CoreNotification::ServerStatus { params } => {
                if params.is_ok() {
                    // todo filter by language
                    self.main_split.docs.with_untracked(|x| {
                        for doc in x.values() {
                            doc.get_code_lens();
                            doc.get_document_symbol();
                            doc.get_semantic_styles();
                            doc.get_folding_range();
                            doc.get_inlay_hints();
                        }
                    });
                }
            }
            CoreNotification::TerminalProcessStopped { term_id, exit_code } => {
                debug!("TerminalProcessStopped {:?}, {:?}", term_id, exit_code);
                if let Err(err) = self
                    .common
                    .term_tx
                    .send((*term_id, TermEvent::CloseTerminal))
                {
                    tracing::error!("{:?}", err);
                }
                self.terminal.terminal_stopped(term_id, *exit_code);
                if self
                    .terminal
                    .tab_info
                    .with_untracked(|info| info.tabs.is_empty())
                {
                    if self.panel.is_panel_visible(&PanelKind::Terminal) {
                        self.panel.hide_panel(&PanelKind::Terminal);
                    }
                    self.common.focus.set(Focus::Workbench);
                }
            }
            CoreNotification::TerminalLaunchFailed { term_id, error } => {
                self.terminal.launch_failed(term_id, error);
            }
            CoreNotification::TerminalProcessId { .. } => {}
            CoreNotification::OpenPaths { paths } => {
                self.open_paths(paths);
            }
            CoreNotification::OpenFileChanged { path, content } => {
                self.main_split.open_file_changed(path, content);
            }
            CoreNotification::WorkDoneProgress { progress } => {
                self.update_progress(progress);
            }
            CoreNotification::ShowMessage { title, message } => {
                self.show_message(title, message);
            }
            CoreNotification::WorkspaceFileChange => {
                self.file_explorer.reload();
            }
            _ => {}
        }
    }

    pub fn key_down<'a>(&self, event: impl Into<EventRef<'a>> + Copy) -> bool {
        if self.alert_data.active.get_untracked() {
            return false;
        }
        let focus = self.common.focus.get_untracked();
        let keypress = self.common.keypress.get_untracked();
        let handle = match focus {
            Focus::Workbench => self.main_split.key_down(event, &keypress),
            Focus::Palette => Some(keypress.key_down(event, &self.palette)),
            Focus::CodeAction => {
                let code_action = self.code_action.get_untracked();
                Some(keypress.key_down(event, &code_action))
            }
            Focus::Rename => Some(keypress.key_down(event, &self.rename)),
            Focus::AboutPopup => Some(keypress.key_down(event, &self.about_data)),
            Focus::Panel(PanelKind::Terminal) => {
                self.terminal.key_down(event, &keypress)
            }
            Focus::Panel(PanelKind::Search) => {
                Some(keypress.key_down(event, &self.global_search))
            }
            Focus::Panel(PanelKind::SourceControl) => {
                Some(keypress.key_down(event, &self.source_control))
            }
            _ => None,
        };

        if let Some(handle) = &handle {
            if handle.handled {
                true
            } else {
                keypress
                    .handle_keymatch(
                        self,
                        handle.keymatch.clone(),
                        handle.keypress.clone(),
                    )
                    .handled
            }
        } else {
            keypress.key_down(event, self).handled
        }
    }

    pub fn workspace_info(&self) -> WorkspaceInfo {
        let main_split_data = self
            .main_split
            .splits
            .get_untracked()
            .get(&self.main_split.root_split)
            .cloned()
            .unwrap();
        WorkspaceInfo {
            split: main_split_data.get_untracked().split_info(self),
            panel: self.panel.panel_info(),
            explorer: Some(self.file_explorer.explorer_info()),
        }
    }

    pub fn hover_origin(&self) -> Option<Point> {
        if !self.common.hover.active.get_untracked() {
            return None;
        }

        let editor_id = self.common.hover.editor_id.get_untracked();
        let editor_data = self.main_split.editors.editor(editor_id)?;

        let (window_origin, viewport, editor) = (
            editor_data.window_origin(),
            editor_data.viewport(),
            &editor_data.editor,
        );

        // TODO(minor): affinity should be gotten from where the hover was started at.
        let (point_above, point_below) = editor.points_of_offset(
            self.common.hover.offset.get_untracked(),
            CursorAffinity::Forward,
        );

        let window_origin =
            window_origin.get() - self.common.window_origin.get().to_vec2();
        let viewport = viewport.get();
        let hover_size = self.common.hover.layout_rect.get().size();
        let tab_size = self.layout_rect.get().size();

        let mut origin = window_origin
            + Vec2::new(
                point_below.x - viewport.x0,
                (point_above.y - viewport.y0) - hover_size.height,
            );
        if origin.y < 0.0 {
            origin.y = window_origin.y + point_below.y - viewport.y0;
        }
        if origin.x + hover_size.width + 1.0 > tab_size.width {
            origin.x = tab_size.width - hover_size.width - 1.0;
        }
        if origin.x <= 0.0 {
            origin.x = 0.0;
        }

        Some(origin)
    }

    pub fn completion_origin(&self) -> Point {
        let completion = self.common.completion.get();
        if completion.status == CompletionStatus::Inactive {
            return Point::ZERO;
        }
        let config = self.common.config.get();
        let editor_data =
            if let Some(editor) = self.main_split.active_editor.get_untracked() {
                editor
            } else {
                return Point::ZERO;
            };

        let (window_origin, viewport, editor) = (
            editor_data.window_origin(),
            editor_data.viewport(),
            &editor_data.editor,
        );

        // TODO(minor): What affinity should we use for this? Probably just use the cursor's
        // original affinity..
        let (point_above, point_below) =
            editor.points_of_offset(completion.offset, CursorAffinity::Forward);

        let window_origin =
            window_origin.get() - self.common.window_origin.get().to_vec2();
        let viewport = viewport.get();
        let completion_size = completion.layout_rect.size();
        let tab_size = self.layout_rect.get().size();

        let mut origin = window_origin
            + Vec2::new(
                point_below.x
                    - viewport.x0
                    - config.editor.line_height() as f64
                    - 5.0,
                point_below.y - viewport.y0,
            );
        if origin.y + completion_size.height > tab_size.height {
            origin.y = window_origin.y + (point_above.y - viewport.y0)
                - completion_size.height;
        }
        if origin.x + completion_size.width + 1.0 > tab_size.width {
            origin.x = tab_size.width - completion_size.width - 1.0;
        }
        if origin.x <= 0.0 {
            origin.x = 0.0;
        }

        origin
    }

    pub fn code_action_origin(&self) -> Point {
        let code_action = self.code_action.get();
        let config = self.common.config.get();
        if code_action.status.get_untracked() == CodeActionStatus::Inactive {
            return Point::ZERO;
        }

        let tab_size = self.layout_rect.get().size();
        let code_action_size = code_action.layout_rect.size();

        let editor_data =
            if let Some(editor) = self.main_split.active_editor.get_untracked() {
                editor
            } else {
                return Point::ZERO;
            };

        let (window_origin, viewport, editor) = (
            editor_data.window_origin(),
            editor_data.viewport(),
            &editor_data.editor,
        );

        // TODO(minor): What affinity should we use for this?
        let (_point_above, point_below) =
            editor.points_of_offset(code_action.offset, CursorAffinity::Forward);

        let window_origin =
            window_origin.get() - self.common.window_origin.get().to_vec2();
        let viewport = viewport.get();

        let mut origin = window_origin
            + Vec2::new(
                if code_action.mouse_click {
                    0.0
                } else {
                    point_below.x - viewport.x0
                },
                point_below.y - viewport.y0,
            );

        if origin.y + code_action_size.height > tab_size.height {
            origin.y = origin.y
                - config.editor.line_height() as f64
                - code_action_size.height;
        }
        if origin.x + code_action_size.width + 1.0 > tab_size.width {
            origin.x = tab_size.width - code_action_size.width - 1.0;
        }
        if origin.x <= 0.0 {
            origin.x = 0.0;
        }

        origin
    }

    pub fn rename_origin(&self) -> Point {
        let config = self.common.config.get();
        if !self.rename.active.get() {
            return Point::ZERO;
        }

        let tab_size = self.layout_rect.get().size();
        let rename_size = self.rename.layout_rect.get().size();

        let editor_data =
            if let Some(editor) = self.main_split.active_editor.get_untracked() {
                editor
            } else {
                return Point::ZERO;
            };

        let (window_origin, viewport, editor) = (
            editor_data.window_origin(),
            editor_data.viewport(),
            &editor_data.editor,
        );

        // TODO(minor): What affinity should we use for this?
        let (_point_above, point_below) = editor.points_of_offset(
            self.rename.start.get_untracked(),
            CursorAffinity::Forward,
        );

        let window_origin =
            window_origin.get() - self.common.window_origin.get().to_vec2();
        let viewport = viewport.get();

        let mut origin = window_origin
            + Vec2::new(point_below.x - viewport.x0, point_below.y - viewport.y0);

        if origin.y + rename_size.height > tab_size.height {
            origin.y =
                origin.y - config.editor.line_height() as f64 - rename_size.height;
        }
        if origin.x + rename_size.width + 1.0 > tab_size.width {
            origin.x = tab_size.width - rename_size.width - 1.0;
        }
        if origin.x <= 0.0 {
            origin.x = 0.0;
        }

        origin
    }

    /// Get the mode for the current editor or terminal
    pub fn mode(&self) -> Mode {
        if self.common.config.get().core.modal {
            let mode = if self.common.focus.get() == Focus::Workbench {
                self.main_split
                    .active_editor
                    .get()
                    .map(|editor| editor.cursor().with(|c| c.get_mode()))
            } else {
                None
            };

            mode.unwrap_or(Mode::Normal)
        } else {
            Mode::Insert
        }
    }

    pub fn toggle_panel_visual(&self, kind: PanelKind) {
        if self.panel.is_panel_visible(&kind) {
            self.hide_panel(kind);
        } else {
            self.show_panel(kind);
        }
    }

    /// Toggle a specific kind of panel.
    fn toggle_panel_focus(&self, kind: PanelKind) {
        let should_hide = match kind {
            PanelKind::Problem
            | PanelKind::CallHierarchy
            | PanelKind::DocumentSymbol
            | PanelKind::GitGraph
            | PanelKind::References
            | PanelKind::Implementation => {
                // Some panels don't accept focus (yet). Fall back to visibility check
                // in those cases.
                self.panel.is_panel_visible(&kind)
            }
            PanelKind::Terminal
            | PanelKind::SourceControl
            | PanelKind::Search
            | PanelKind::FileExplorer => self.is_panel_focused(kind),
        };
        if should_hide {
            self.hide_panel(kind);
        } else {
            self.show_panel(kind);
        }
    }

    /// Toggle a panel on one of the sides.
    fn toggle_container_visual(&self, position: &PanelContainerPosition) {
        let shown = !self.panel.is_container_shown(position, false);
        self.panel.set_shown(&position.first(), shown);
        self.panel.set_shown(&position.second(), shown);

        if shown {
            if let Some((kind, _)) = self
                .panel
                .active_panel_at_position(&position.second(), false)
            {
                self.show_panel(kind);
            }

            if let Some((kind, _)) = self
                .panel
                .active_panel_at_position(&position.first(), false)
            {
                self.show_panel(kind);
            }
        } else {
            if let Some((kind, _)) = self
                .panel
                .active_panel_at_position(&position.second(), false)
            {
                self.hide_panel(kind);
            }

            if let Some((kind, _)) = self
                .panel
                .active_panel_at_position(&position.first(), false)
            {
                self.hide_panel(kind);
            }
        }
    }

    fn is_panel_focused(&self, kind: PanelKind) -> bool {
        // Moving between e.g. Search and Problems doesn't affect focus, so we need to also check
        // visibility.
        self.common.focus.get_untracked() == Focus::Panel(kind)
            && self.panel.is_panel_visible(&kind)
    }

    fn hide_panel(&self, kind: PanelKind) {
        self.panel.hide_panel(&kind);
        self.common.focus.set(Focus::Workbench);
    }

    pub fn show_panel(&self, kind: PanelKind) {
        if kind == PanelKind::Terminal
            && self
                .terminal
                .tab_info
                .with_untracked(|info| info.tabs.is_empty())
        {
            self.terminal.new_tab(
                self.common
                    .config
                    .get_untracked()
                    .terminal
                    .get_default_profile(),
            );
        }
        self.panel.show_panel(&kind);
        if kind == PanelKind::Search
            && self.common.focus.get_untracked() == Focus::Workbench
        {
            let active_editor = self.main_split.active_editor.get_untracked();
            let word = active_editor.map(|editor| editor.word_at_cursor());
            if let Some(word) = word {
                if !word.is_empty() {
                    self.global_search.set_pattern(word);
                }
            }
        }
        self.common.focus.set(Focus::Panel(kind));
    }

    /// The terminal that Run restart/stop act on: the focused terminal,
    /// if it is running a run configuration.
    fn active_run_term(&self) -> Option<TermId> {
        let terminal = self.terminal.active_tab(false)?.active_terminal(false)?;
        terminal
            .run_debug
            .with_untracked(|run| run.is_some())
            .then_some(terminal.term_id)
    }

    fn run_in_terminal(&self, config: &RunConfig) {
        debug!("{:?}", config);
        let process = RunProcess {
            config: config.clone(),
            stopped: false,
            created: Instant::now(),
            is_prelaunch: true,
        };
        let term_id =
            if let Some(terminal) = self.terminal.get_stopped_run_terminal(config) {
                terminal.new_process(Some(process));
                terminal.term_id
            } else {
                let new_terminal_tab =
                    self.terminal.new_tab_run_debug(Some(process), None);
                new_terminal_tab.active_terminal(false).unwrap().term_id
            };
        self.common.focus.set(Focus::Panel(PanelKind::Terminal));
        self.terminal.focus_terminal(term_id);

        if !self.panel.is_panel_visible(&PanelKind::Terminal) {
            self.panel.show_panel(&PanelKind::Terminal);
        }
    }

    pub fn open_paths(&self, paths: &[PathObject]) {
        let (folders, files): (Vec<&PathObject>, Vec<&PathObject>) = paths
            .iter()
            .partition(|p| p.is_dir || PlanckWorkspace::is_space_file(&p.path));

        for folder in folders {
            self.common.window_common.window_command.send(
                WindowCommand::NewWorkspaceTab {
                    workspace: PlanckWorkspace::from_path(folder.path.clone()),
                    end: false,
                },
            );
        }

        for file in files {
            let position = file.linecol.map(|pos| {
                EditorPosition::Position(lsp_types::Position {
                    line: pos.line.saturating_sub(1) as u32,
                    character: pos.column.saturating_sub(1) as u32,
                })
            });

            self.common
                .internal_command
                .send(InternalCommand::GoToLocation {
                    location: EditorLocation {
                        path: file.path.clone(),
                        position,
                        scroll_offset: None,
                        // Create a new editor for the file, so we don't change any current unconfirmed
                        // editor
                        ignore_unconfirmed: true,
                        same_editor_tab: false,
                    },
                });
        }
    }

    pub fn show_alert(&self, title: String, msg: String, buttons: Vec<AlertButton>) {
        self.alert_data.title.set(title);
        self.alert_data.msg.set(msg);
        self.alert_data.buttons.set(buttons);
        self.alert_data.active.set(true);
    }

    fn update_progress(&self, progress: &ProgressParams) {
        let token = progress.token.clone();
        match &progress.value {
            lsp_types::ProgressParamsValue::WorkDone(progress) => match progress {
                lsp_types::WorkDoneProgress::Begin(progress) => {
                    let progress = WorkProgress {
                        token: token.clone(),
                        title: progress.title.clone(),
                        message: progress.message.clone(),
                        percentage: progress.percentage,
                    };
                    self.progresses.update(|p| {
                        p.insert(token, progress);
                    });
                }
                lsp_types::WorkDoneProgress::Report(report) => {
                    self.progresses.update(|p| {
                        if let Some(progress) = p.get_mut(&token) {
                            progress.message.clone_from(&report.message);
                            progress.percentage = report.percentage;
                        }
                    })
                }
                lsp_types::WorkDoneProgress::End(_) => {
                    self.progresses.update(|p| {
                        p.swap_remove(&token);
                    });
                }
            },
        }
    }

    fn show_message(&self, title: &str, message: &ShowMessageParams) {
        self.messages.update(|messages| {
            messages.push((title.to_string(), message.clone()));
        });
    }

    /// Links `/usr/local/bin/planck` to this Planck, or removes the link, so
    /// `planck file.txt` in a terminal opens the file here. macOS asks for an
    /// admin password, since `/usr/local/bin` belongs to root.
    #[cfg(target_os = "macos")]
    fn shell_command(&self, install: bool) {
        use std::env;

        use lsp_types::MessageType;

        const LINK: &str = "/usr/local/bin/planck";
        let messages = self.messages;
        let show = move |typ: MessageType, message: String| {
            messages.update(|messages| {
                messages.push((
                    "Shell command".to_string(),
                    ShowMessageParams { typ, message },
                ));
            });
        };

        // Resolved, so running Planck through the link doesn't link it to itself.
        let exe = match env::current_exe().and_then(|p| p.canonicalize()) {
            Ok(exe) => exe,
            Err(err) => {
                show(MessageType::ERROR, format!("Can't find Planck: {err}"));
                return;
            }
        };
        let linked = std::fs::read_link(LINK).ok();
        let script = if install {
            let path = exe.to_string_lossy();
            // A link into the disk image or a translocated copy breaks later.
            if path.starts_with("/Volumes/") || path.contains("/AppTranslocation/") {
                show(
                    MessageType::WARNING,
                    "Move Planck to the Applications folder first, then install \
                     the shell command again."
                        .to_string(),
                );
                return;
            }
            if linked.as_ref() == Some(&exe) {
                show(
                    MessageType::INFO,
                    "Already installed: `planck file.txt` opens the file in Planck."
                        .to_string(),
                );
                return;
            }
            r#"do shell script "mkdir -p /usr/local/bin && ln -sf " & quoted form of item 1 of argv & " /usr/local/bin/planck" with administrator privileges"#
        } else {
            // Only a link is removed, never a `planck` someone put there by hand.
            if linked.is_none() {
                show(MessageType::INFO, "`planck` isn't installed.".to_string());
                return;
            }
            r#"do shell script "rm -f /usr/local/bin/planck" with administrator privileges"#
        };

        // `Err(None)`: the password prompt was cancelled, so nothing is shown.
        let send = create_ext_action(
            self.common.scope,
            move |result: Result<(), Option<String>>| match result {
                Ok(()) if install => show(
                    MessageType::INFO,
                    "Installed: `planck file.txt` in a terminal opens the file in \
                     Planck."
                        .to_string(),
                ),
                Ok(()) => show(MessageType::INFO, "`planck` removed.".to_string()),
                Err(Some(err)) => show(MessageType::ERROR, err),
                Err(None) => {}
            },
        );
        std::thread::Builder::new()
            .name("ShellCommand".to_owned())
            .spawn(move || {
                // The path goes in as an argument, so no quote in it can break the script.
                let output = std::process::Command::new("osascript")
                    .args(["-e", "on run argv", "-e", script, "-e", "end run"])
                    .arg(&exe)
                    .output();
                send(match output {
                    Ok(out) if out.status.success() => Ok(()),
                    Ok(out) => {
                        let err = String::from_utf8_lossy(&out.stderr);
                        Err((!err.contains("(-128)"))
                            .then(|| err.trim().to_string()))
                    }
                    Err(err) => Err(Some(err.to_string())),
                });
            })
            .unwrap();
    }

    pub fn update_code_lens_id(&self, view_id: Option<ViewId>) {
        if let Some(Some(old_id)) = self.code_lens.try_update(|x| {
            let old = x.take();
            if let Some(id) = view_id {
                let _ = x.insert(id);
            }
            old
        }) {
            remove_overlay(old_id);
        }
    }

    pub fn show_code_lens(
        &self,
        mouse_click: bool,
        plugin_id: PluginId,
        offset: usize,
        lens: im::Vector<CodeLens>,
    ) {
        self.common
            .internal_command
            .send(InternalCommand::ShowCodeActions {
                offset,
                mouse_click,
                plugin_id,
                code_actions: lens
                    .into_iter()
                    .filter_map(|lens| {
                        Some(CodeActionOrCommand::Command(lens.command?))
                    })
                    .collect(),
            });
    }

    pub fn call_hierarchy_incoming(&self, item_id: ViewId) {
        let Some(root) = self.call_hierarchy_data.root.get_untracked() else {
            return;
        };
        let Some(item) = CallHierarchyItemData::find_by_id(root, item_id) else {
            return;
        };
        let root_item = item;
        let path: PathBuf = item.get_untracked().item.uri.to_file_path().unwrap();
        let scope = self.scope;
        let send =
            create_ext_action(scope, move |_rs: Result<ProxyResponse, RpcError>| {
                match _rs {
                    Ok(ProxyResponse::CallHierarchyIncomingResponse { items }) => {
                        if let Some(items) = items {
                            let mut item_children = Vec::new();
                            for x in items {
                                let item = Rc::new(x.from);
                                for range in x.from_ranges {
                                    item_children.push(scope.create_rw_signal(
                                        CallHierarchyItemData {
                                            view_id: floem::ViewId::new(),
                                            item: item.clone(),
                                            from_range: range,
                                            init: false,
                                            open: scope.create_rw_signal(false),
                                            children:
                                                scope.create_rw_signal(Vec::new()),
                                        },
                                    ))
                                }
                            }
                            root_item.update(|x| {
                                x.init = true;
                                x.children.update(|children| {
                                    *children = item_children;
                                })
                            });
                        }
                    }
                    Err(err) => {
                        tracing::error!("{:?}", err);
                    }
                    Ok(_) => {}
                }
            });
        self.common.proxy.call_hierarchy_incoming(
            path,
            item.get_untracked().item.as_ref().clone(),
            send,
        );
    }
}

/// Open path with the default application without blocking.
fn open_uri(path: &Path) {
    match open::that(path) {
        Ok(_) => {
            debug!("opened active file: {path:?}");
        }
        Err(e) => {
            error!("failed to open active file: {path:?}, error: {e}");
        }
    }
}
