use std::{
    borrow::Cow,
    collections::HashMap,
    ffi::OsStr,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
};

use floem::{
    action::show_context_menu,
    event::EventPropagation,
    ext_event::create_ext_action,
    keyboard::Modifiers,
    menu::{Menu, MenuItem},
    reactive::{ReadSignal, RwSignal, Scope, SignalGet, SignalUpdate, SignalWith},
    views::editor::text::SystemClipboard,
};
use globset::Glob;
use planck_core::{
    command::{EditCommand, FocusCommand},
    mode::Mode,
    register::Clipboard,
};
use planck_rpc::{
    file::{
        Duplicating, FileNodeItem, FileNodeViewKind, Naming, NamingState, NewNode,
        Renaming,
    },
    proxy::{PathRequest, ProxyResponse},
};

use crate::{
    command::{
        CommandExecuted, CommandKind, InternalCommand, PlanckCommand, WindowCommand,
    },
    config::PlanckConfig,
    editor::EditorData,
    keypress::{KeyPressFocus, condition::Condition},
    main_split::Editors,
    window_tab::CommonData,
    workspace::{ExplorerInfo, PlanckWorkspace, remove_space_folder},
};

enum RenamedPath {
    NotRenaming,
    NameUnchanged,
    Renamed {
        current_path: PathBuf,
        new_path: PathBuf,
    },
}

/// The root folder whose tree holds `path`.
// shortcut: with nested space folders the innermost root wins, so the outer tree cannot
// expand into the inner folder; track the root per row if nested folders matter.
fn root_of<'a>(roots: &'a [FileNodeItem], path: &Path) -> Option<&'a FileNodeItem> {
    roots
        .iter()
        .filter(|r| path.starts_with(&r.path))
        .max_by_key(|r| r.path.components().count())
}

fn root_of_mut<'a>(
    roots: &'a mut [FileNodeItem],
    path: &Path,
) -> Option<&'a mut FileNodeItem> {
    roots
        .iter_mut()
        .filter(|r| path.starts_with(&r.path))
        .max_by_key(|r| r.path.components().count())
}

#[derive(Clone, Debug)]
pub struct FileExplorerData {
    /// One tree per root folder: the space's folders, or the single open folder.
    pub roots: RwSignal<Vec<FileNodeItem>>,
    pub naming: RwSignal<Naming>,
    pub naming_editor_data: EditorData,
    pub common: Rc<CommonData>,
    pub scroll_to_line: RwSignal<Option<f64>>,
    left_diff_path: RwSignal<Option<PathBuf>>,
    pub select: RwSignal<Option<FileNodeViewKind>>,
}

impl KeyPressFocus for FileExplorerData {
    fn get_mode(&self) -> Mode {
        Mode::Insert
    }

    fn check_condition(&self, condition: Condition) -> bool {
        (self.naming.with_untracked(Naming::is_accepting_input)
            && condition == Condition::ModalFocus)
            || matches!(
                condition,
                Condition::PanelFocus | Condition::FileExplorerFocus
            )
    }

    fn run_command(
        &self,
        command: &PlanckCommand,
        count: Option<usize>,
        mods: Modifiers,
    ) -> CommandExecuted {
        if self.naming.with_untracked(Naming::is_accepting_input) {
            match command.kind {
                CommandKind::Focus(FocusCommand::ModalClose) => {
                    self.cancel_naming();
                    CommandExecuted::Yes
                }
                CommandKind::Edit(EditCommand::InsertNewLine) => {
                    self.finish_naming();
                    CommandExecuted::Yes
                }
                CommandKind::Edit(_) => {
                    let command_executed =
                        self.naming_editor_data.run_command(command, count, mods);

                    if let Some(new_path) = self.naming_path() {
                        self.common
                            .internal_command
                            .send(InternalCommand::TestPathCreation { new_path });
                    }

                    command_executed
                }
                _ => self.naming_editor_data.run_command(command, count, mods),
            }
        } else {
            CommandExecuted::No
        }
    }

    fn receive_char(&self, c: &str) {
        if self.naming.with_untracked(Naming::is_accepting_input) {
            self.naming_editor_data.receive_char(c);

            if let Some(new_path) = self.naming_path() {
                self.common
                    .internal_command
                    .send(InternalCommand::TestPathCreation { new_path });
            }
        }
    }
}

impl FileExplorerData {
    pub fn new(
        cx: Scope,
        editors: Editors,
        common: Rc<CommonData>,
        saved: Option<&ExplorerInfo>,
    ) -> Self {
        let folders = common.workspace.folder_paths();
        let roots = cx.create_rw_signal(
            folders
                .iter()
                .map(|path| FileNodeItem {
                    path: path.clone(),
                    is_dir: true,
                    read: false,
                    open: false,
                    children: HashMap::new(),
                    children_open_count: 0,
                })
                .collect(),
        );
        let naming = cx.create_rw_signal(Naming::None);
        let naming_editor_data = editors.make_local(cx, common.clone());
        let data = Self {
            roots,
            naming,
            naming_editor_data,
            common,
            scroll_to_line: cx.create_rw_signal(None),
            left_diff_path: cx.create_rw_signal(None),
            select: cx.create_rw_signal(None),
        };
        if let Some(saved) = saved {
            data.restore_expanded(saved.expanded.clone());
            data.select
                .set(saved.selected.clone().map(FileNodeViewKind::Path));
        } else if let [path] = folders.as_slice() {
            // A space with several folders starts collapsed so every root stays in view.
            data.toggle_expand(path);
        }
        data
    }

    /// The open folders and the selected item, saved with the session.
    pub fn explorer_info(&self) -> ExplorerInfo {
        fn open_dirs(node: &FileNodeItem, out: &mut Vec<PathBuf>) {
            if node.is_dir && node.open {
                out.push(node.path.clone());
                for child in node.children.values() {
                    open_dirs(child, out);
                }
            }
        }
        let mut expanded = Vec::new();
        self.roots.with_untracked(|roots| {
            for root in roots {
                open_dirs(root, &mut expanded);
            }
        });
        let selected = match self.select.get_untracked() {
            Some(FileNodeViewKind::Path(path)) => Some(path),
            _ => None,
        };
        ExplorerInfo { expanded, selected }
    }

    /// Opens the saved folders. A folder inside one that is not read yet only
    /// exists once its parent is read, so it waits for that read and retries.
    fn restore_expanded(&self, dirs: Vec<PathBuf>) {
        let (to_read, pending) = self
            .roots
            .try_update(|roots| {
                let mut to_read = Vec::new();
                let mut pending = Vec::new();
                for dir in dirs {
                    let Some(root) = root_of_mut(roots, &dir) else {
                        continue;
                    };
                    match root.get_file_node_mut(&dir) {
                        Some(node) if node.is_dir => {
                            node.open = true;
                            if !node.read {
                                to_read.push(dir.clone());
                            }
                            root.update_node_count_recursive(&dir);
                        }
                        Some(_) => {}
                        None => pending.push(dir),
                    }
                }
                (to_read, pending)
            })
            .unwrap_or_default();
        for dir in to_read {
            let data = self.clone();
            let pending = pending.clone();
            self.read_dir_cb(&dir, move |_| {
                if !pending.is_empty() {
                    data.restore_expanded(pending);
                }
            });
        }
    }

    /// Rename the currently selected file, if it exists.
    pub fn rename_selected(&self) {
        if let Some(file) = self.select.get_untracked()
            && let Some(path) = file.path()
        {
            self.naming.set(Naming::Renaming(Renaming {
                state: NamingState::Naming,
                path: path.to_path_buf(),
                editor_needs_reset: true,
            }));
        }
    }

    /// Trash the currently selected file, if it exists.
    pub fn trash_selected(&self) {
        if let Some(file) = self.select.get_untracked()
            && let Some(path) = file.path()
        {
            self.common.proxy.trash_path(path.to_path_buf(), |res| {
                if let Err(err) = res {
                    tracing::warn!("Failed to trash path: {:?}", err);
                }
            })
        }
    }

    /// Reload the file explorer data via reading the root directories.
    /// Note that this will not update immediately.
    pub fn reload(&self) {
        let paths: Vec<PathBuf> = self
            .roots
            .with_untracked(|roots| roots.iter().map(|r| r.path.clone()).collect());
        for path in paths {
            self.read_dir(&path);
        }
    }

    /// Toggle whether the directory is expanded or not.  
    /// Does nothing if the path does not exist or is not a directory.
    pub fn toggle_expand(&self, path: &Path) {
        let Some(Some(read)) = self.roots.try_update(|roots| {
            let root = root_of_mut(roots, path)?;
            let read = if let Some(node) = root.get_file_node_mut(path) {
                if !node.is_dir {
                    return None;
                }
                node.open = !node.open;
                Some(node.read)
            } else {
                None
            };

            if Some(true) == read {
                root.update_node_count_recursive(path);
            }
            read
        }) else {
            return;
        };

        // Read the directory's files if they haven't been read yet
        if !read {
            self.read_dir(path);
        }
    }

    pub fn read_dir(&self, path: &Path) {
        self.read_dir_cb(path, |_| {});
    }

    /// Read the directory's information and update the file explorer tree.  
    /// `done : FnOnce(was_read: bool)` is called when the operation is completed, whether success,
    /// failure, or ignored.
    pub fn read_dir_cb(&self, path: &Path, done: impl FnOnce(bool) + 'static) {
        let roots = self.roots;
        let data = self.clone();
        let config = self.common.config;
        let send = {
            let path = path.to_path_buf();
            create_ext_action(self.common.scope, move |result| {
                let Ok(ProxyResponse::ReadDirResponse { mut items, .. }) = result
                else {
                    done(false);
                    return;
                };

                roots.update(|roots| {
                    let Some(root) = root_of_mut(roots, &path) else {
                        return;
                    };
                    // Get the node for this path, which should already exist if we're calling
                    // read_dir on it.
                    if let Some(node) = root.get_file_node_mut(&path) {
                        // TODO: do not recreate glob every time we read a directory
                        // Retain only items that are not excluded from view by the configuration
                        match Glob::new(&config.get().editor.files_exclude) {
                            Ok(glob) => {
                                let matcher = glob.compile_matcher();
                                items.retain(|i| !matcher.is_match(&i.path));
                            }
                            Err(e) => tracing::error!(
                                target:"files_exclude",
                                "Failed to compile glob: {}",
                                e
                            ),
                        }

                        node.read = true;

                        // Remove paths that no longer exist
                        let removed_paths: Vec<PathBuf> = node
                            .children
                            .keys()
                            .filter(|p| !items.iter().any(|i| &&i.path == p))
                            .map(PathBuf::from)
                            .collect();
                        for path in removed_paths {
                            node.children.remove(&path);
                        }

                        // Reread dirs that were already read and add new paths
                        for item in items {
                            if let Some(existing) = node.children.get(&item.path) {
                                if existing.read {
                                    data.read_dir(&existing.path);
                                }
                            } else {
                                node.children.insert(item.path.clone(), item);
                            }
                        }
                    }
                    root.update_node_count_recursive(&path);
                });

                done(true);
            })
        };

        // Ask the proxy for the directory information
        self.common
            .proxy
            .read_dir(PathRequest::Path(path.to_path_buf()), send);
    }

    /// Returns `true` if `path` exists in the file explorer tree and is a directory, `false`
    /// otherwise.
    fn is_dir(&self, path: &Path) -> bool {
        self.roots.with_untracked(|roots| {
            root_of(roots, path)
                .and_then(|root| root.get_file_node(path))
                .is_some_and(|node| node.is_dir)
        })
    }

    /// The current path that we're renaming to / creating or duplicating a node at.  
    /// Note: returns `None` when renaming if the file name has not changed.
    fn naming_path(&self) -> Option<PathBuf> {
        self.naming.with_untracked(|naming| match naming {
            Naming::None => None,
            Naming::Renaming(_) => {
                if let RenamedPath::Renamed { new_path, .. } = self.renamed_path() {
                    Some(new_path)
                } else {
                    None
                }
            }
            Naming::NewNode(n) => {
                let relative_path = self.naming_editor_data.text();
                let relative_path: Cow<OsStr> = match relative_path.slice_to_cow(..)
                {
                    Cow::Borrowed(path) => Cow::Borrowed(path.as_ref()),
                    Cow::Owned(path) => Cow::Owned(path.into()),
                };

                Some(n.base_path.join(relative_path))
            }
            Naming::Duplicating(d) => {
                let relative_path = self.naming_editor_data.text();
                let relative_path: Cow<OsStr> = match relative_path.slice_to_cow(..)
                {
                    Cow::Borrowed(path) => Cow::Borrowed(path.as_ref()),
                    Cow::Owned(path) => Cow::Owned(path.into()),
                };

                let new_path =
                    d.path.parent().unwrap_or("".as_ref()).join(relative_path);

                Some(new_path)
            }
        })
    }

    /// If there is an in progress rename and the user has entered a path that differs from the
    /// current path, gets the current and new paths of the renamed node.
    fn renamed_path(&self) -> RenamedPath {
        self.naming.with_untracked(|naming| {
            if let Some(current_path) = naming.as_renaming().map(|x| &x.path) {
                let current_file_name = current_path.file_name().unwrap_or_default();
                // `new_relative_path` is the new path relative to the parent directory, unless the
                // user has entered an absolute path.
                let new_relative_path = self.naming_editor_data.text();

                let new_relative_path: Cow<OsStr> =
                    match new_relative_path.slice_to_cow(..) {
                        Cow::Borrowed(path) => Cow::Borrowed(path.as_ref()),
                        Cow::Owned(path) => Cow::Owned(path.into()),
                    };

                if new_relative_path == current_file_name {
                    RenamedPath::NameUnchanged
                } else {
                    let new_path = current_path
                        .parent()
                        .unwrap_or("".as_ref())
                        .join(new_relative_path);

                    RenamedPath::Renamed {
                        current_path: current_path.to_owned(),
                        new_path,
                    }
                }
            } else {
                RenamedPath::NotRenaming
            }
        })
    }

    /// If a naming is in progress and the user has entered a valid path, send the request to
    /// actually perform the change.
    pub fn finish_naming(&self) {
        match self.naming.get_untracked() {
            Naming::None => {}
            Naming::Renaming(_) => {
                let renamed_path = self.renamed_path();
                match renamed_path {
                    // Should not occur
                    RenamedPath::NotRenaming => {}
                    RenamedPath::NameUnchanged => {
                        self.cancel_naming();
                    }
                    RenamedPath::Renamed {
                        current_path,
                        new_path,
                    } => {
                        self.common.internal_command.send(
                            InternalCommand::FinishRenamePath {
                                current_path,
                                new_path,
                            },
                        );
                    }
                }
            }
            Naming::NewNode(n) => {
                let Some(path) = self.naming_path() else {
                    return;
                };

                self.common
                    .internal_command
                    .send(InternalCommand::FinishNewNode {
                        is_dir: n.is_dir,
                        path,
                    });
            }
            Naming::Duplicating(d) => {
                let Some(path) = self.naming_path() else {
                    return;
                };

                self.common.internal_command.send(
                    InternalCommand::FinishDuplicate {
                        source: d.path.to_owned(),
                        path,
                    },
                );
            }
        }
    }

    /// Closes the naming text box without applying the effect.
    pub fn cancel_naming(&self) {
        self.naming.set(Naming::None);
    }

    pub fn click(&self, path: &Path, config: ReadSignal<Arc<PlanckConfig>>) {
        if self.is_dir(path) {
            self.toggle_expand(path);
        } else if !config.get_untracked().core.file_explorer_double_click {
            self.common
                .internal_command
                .send(InternalCommand::OpenFile {
                    path: path.to_path_buf(),
                })
        }
    }

    pub fn reveal_in_file_tree(&self, path: PathBuf) {
        let done = self
            .roots
            .try_update(|roots| {
                let Some(root) = root_of_mut(roots, &path) else {
                    return false;
                };
                // the directories in which the file are located are all read and opened
                if root.get_file_node(&path).is_some() {
                    for current_path in path.ancestors() {
                        if let Some(file) = root.get_file_node_mut(current_path)
                            && file.is_dir
                        {
                            file.open = true;
                        }
                    }
                    root.update_node_count_recursive(&path);
                    true
                } else {
                    // read and open the directories in which the file are located
                    let mut read_dir = None;
                    // Whether the file is in the workspace
                    let mut exist = false;
                    for current_path in path.ancestors() {
                        if let Some(file) = root.get_file_node_mut(current_path) {
                            exist = true;
                            if file.is_dir {
                                file.open = true;
                                if !file.read {
                                    read_dir = Some(current_path.to_path_buf())
                                }
                            }
                            break;
                        }
                    }
                    if let (true, Some(dir)) = (exist, read_dir) {
                        let explorer = self.clone();
                        let select_path = path.clone();
                        self.read_dir_cb(&dir, move |_| {
                            explorer.reveal_in_file_tree(select_path);
                        })
                    }
                    false
                }
            })
            .unwrap_or(false);
        if done {
            let (found, line) = self.roots.with_untracked(|roots| {
                // Each root takes its own row plus its open descendants.
                let mut offset = 0.0;
                for root in roots {
                    let (found, line) = root.find_file_at_line(&path);
                    if found {
                        return (true, offset + line);
                    }
                    offset += (root.children_open_count + 1) as f64;
                }
                (false, 0.0)
            });
            if found {
                self.scroll_to_line.set(Some(line));
                self.select.set(Some(FileNodeViewKind::Path(path)));
            }
        }
    }

    pub fn double_click(
        &self,
        path: &Path,
        config: ReadSignal<Arc<PlanckConfig>>,
    ) -> EventPropagation {
        if self.is_dir(path) {
            EventPropagation::Continue
        } else if config.get_untracked().core.file_explorer_double_click {
            self.common.internal_command.send(
                InternalCommand::OpenAndConfirmedFile {
                    path: path.to_path_buf(),
                },
            );
            EventPropagation::Stop
        } else {
            self.common
                .internal_command
                .send(InternalCommand::MakeConfirmed);
            EventPropagation::Stop
        }
    }

    pub fn secondary_click(&self, path: &Path) {
        let common = self.common.clone();
        let path_a = path.to_owned();
        let left_diff_path = self.left_diff_path;
        // TODO: should we just pass is_dir into secondary click?
        let is_dir = self.is_dir(path);

        let Some(root_path) = self
            .roots
            .with_untracked(|roots| root_of(roots, path).map(|r| r.path.clone()))
        else {
            // There is no context menu outside the workspace folders
            return;
        };

        let is_workspace = path == root_path;

        let base_path_a = if is_dir {
            Some(path_a.clone())
        } else {
            path_a.parent().map(ToOwned::to_owned)
        };
        let base_path_a = base_path_a.as_ref().unwrap_or(&root_path);

        let mut menu = Menu::new("");

        let base_path = base_path_a.clone();
        let data = self.clone();
        let naming = self.naming;
        menu = menu.entry(MenuItem::new("New File").action(move || {
            let base_path_b = &base_path;
            let base_path = base_path.clone();
            data.read_dir_cb(base_path_b, move |was_read| {
                if !was_read {
                    tracing::warn!(
                        "Failed to read directory, avoiding creating node in: {:?}",
                        base_path
                    );
                    return;
                }

                naming.set(Naming::NewNode(NewNode {
                    state: NamingState::Naming,
                    base_path: base_path.clone(),
                    is_dir: false,
                    editor_needs_reset: true,
                }));
            });
        }));

        let base_path = base_path_a.clone();
        let data = self.clone();
        let naming = self.naming;
        menu = menu.entry(MenuItem::new("New Directory").action(move || {
            let base_path_b = &base_path;
            let base_path = base_path.clone();
            data.read_dir_cb(base_path_b, move |was_read| {
                if !was_read {
                    tracing::warn!(
                        "Failed to read directory, avoiding creating node in: {:?}",
                        base_path
                    );
                    return;
                }

                naming.set(Naming::NewNode(NewNode {
                    state: NamingState::Naming,
                    base_path: base_path.clone(),
                    is_dir: true,
                    editor_needs_reset: true,
                }));
            })
        }));

        menu = menu.separator();

        {
            let path = path_a.clone();
            #[cfg(not(target_os = "macos"))]
            let title = "Reveal in system file explorer";
            #[cfg(target_os = "macos")]
            let title = "Reveal in Finder";
            menu = menu.entry(MenuItem::new(title).action(move || {
                let path = path.parent().unwrap_or(&path);
                if !path.exists() {
                    return;
                }

                if let Err(err) = open::that(path) {
                    tracing::error!(
                        "Failed to reveal file in system file explorer: {}",
                        err
                    );
                }
            }));
        }

        {
            let dir = base_path_a.clone();
            let common = common.clone();
            menu = menu.entry(MenuItem::new("Open in Terminal").action(move || {
                let profile = planck_rpc::terminal::TerminalProfile {
                    workdir: url::Url::from_file_path(&dir).ok(),
                    ..common
                        .config
                        .get_untracked()
                        .terminal
                        .get_default_profile()
                        .unwrap_or_default()
                };
                common.internal_command.send(InternalCommand::NewTerminal {
                    profile: Some(profile),
                });
            }));
        }

        if !is_workspace {
            let path = path_a.clone();
            menu = menu.entry(MenuItem::new("Rename").action(move || {
                naming.set(Naming::Renaming(Renaming {
                    state: NamingState::Naming,
                    path: path.clone(),
                    editor_needs_reset: true,
                }));
            }));

            let path = path_a.clone();
            menu = menu.entry(MenuItem::new("Duplicate").action(move || {
                naming.set(Naming::Duplicating(Duplicating {
                    state: NamingState::Naming,
                    path: path.clone(),
                    editor_needs_reset: true,
                }));
            }));

            // TODO: it is common for shift+right click to make 'Move file to trash' an actual
            // Delete, which can be useful for large files.
            let path = path_a.clone();
            let proxy = common.proxy.clone();
            let trash_text = if is_dir {
                "Move Directory to Trash"
            } else {
                "Move File to Trash"
            };
            menu = menu.entry(MenuItem::new(trash_text).action(move || {
                proxy.trash_path(path.clone(), |res| {
                    if let Err(err) = res {
                        tracing::warn!("Failed to trash path: {:?}", err);
                    }
                })
            }));
        }

        if let (true, Some(space)) = (is_workspace, common.workspace.space.clone()) {
            let path = path_a.clone();
            let window_command = common.window_common.window_command;
            menu = menu.entry(MenuItem::new("Remove Folder from Space").action(
                move || {
                    if let Err(err) = remove_space_folder(&space, &path) {
                        tracing::error!(
                            "failed to remove {path:?} from {space:?}: {err:?}"
                        );
                        return;
                    }
                    window_command.send(WindowCommand::SetWorkspace {
                        workspace: PlanckWorkspace::from_path(space.clone()),
                    });
                },
            ));
        }

        menu = menu.separator();

        let path = path_a.clone();
        menu = menu.entry(MenuItem::new("Copy Path").action(move || {
            let mut clipboard = SystemClipboard::new();
            clipboard.put_string(path.to_string_lossy());
        }));

        let path = path_a.clone();
        menu = menu.entry(MenuItem::new("Copy Relative Path").action(move || {
            let relative_path = path.strip_prefix(&root_path).unwrap_or(&path);

            let mut clipboard = SystemClipboard::new();
            clipboard.put_string(relative_path.to_string_lossy());
        }));

        menu = menu.separator();

        let path = path_a.clone();
        menu = menu.entry(
            MenuItem::new("Select for Compare")
                .action(move || left_diff_path.set(Some(path.clone()))),
        );

        if let Some(left_path) = self.left_diff_path.get_untracked() {
            let common = self.common.clone();
            let right_path = path_a.to_owned();

            menu = menu.entry(MenuItem::new("Compare with Selected").action(
                move || {
                    common
                        .internal_command
                        .send(InternalCommand::OpenDiffFiles {
                            left_path: left_path.clone(),
                            right_path: right_path.clone(),
                        })
                },
            ))
        }

        menu = menu.separator();

        let internal_command = common.internal_command;
        menu = menu.entry(MenuItem::new("Refresh").action(move || {
            internal_command.send(InternalCommand::ReloadFileExplorer);
        }));

        show_context_menu(menu, None);
    }

    pub fn middle_click(&self, path: &Path) -> EventPropagation {
        if self.is_dir(path) {
            EventPropagation::Continue
        } else {
            self.common
                .internal_command
                .send(InternalCommand::OpenFileInNewTab {
                    path: path.to_path_buf(),
                });
            EventPropagation::Stop
        }
    }
}
