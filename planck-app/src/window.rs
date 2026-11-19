use std::{rc::Rc, sync::Arc};

use floem::{
    ViewId,
    action::TimerToken,
    peniko::kurbo::{Point, Size},
    reactive::{
        Memo, RwSignal, Scope, SignalGet, SignalUpdate, SignalWith, use_context,
    },
    window::WindowId,
};
use serde::{Deserialize, Serialize};

use crate::{
    app::AppCommand,
    command::{InternalCommand, WindowCommand},
    config::PlanckConfig,
    db::PlanckDb,
    editor::EditorInfo,
    keypress::EventRef,
    listener::Listener,
    main_split::CarriedTabs,
    window_tab::WindowTabData,
    workspace::PlanckWorkspace,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TabsInfo {
    pub active_tab: usize,
    pub workspaces: Vec<PlanckWorkspace>,
}

impl TabsInfo {
    /// Adds a tab for `workspace` unless one is open on its folder or space, and
    /// returns the index of its tab. Two tabs on one workspace would share its
    /// saved session, and the one saved last would drop the other's Untitled files.
    pub fn add(&mut self, workspace: PlanckWorkspace) -> usize {
        if let Some(index) =
            self.workspaces.iter().position(|w| w.is_same(&workspace))
        {
            return index;
        }
        self.workspaces.push(workspace);
        self.workspaces.len() - 1
    }

    /// The same tabs with each workspace once, the active one still active.
    fn deduped(self) -> Self {
        let mut tabs = TabsInfo {
            active_tab: 0,
            workspaces: Vec::new(),
        };
        for (i, workspace) in self.workspaces.into_iter().enumerate() {
            let index = tabs.add(workspace);
            if i == self.active_tab {
                tabs.active_tab = index;
            }
        }
        tabs
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowInfo {
    pub size: Size,
    pub pos: Point,
    pub maximised: bool,
    pub tabs: TabsInfo,
}

#[derive(Clone)]
pub struct WindowCommonData {
    pub window_command: Listener<WindowCommand>,
    pub window_scale: RwSignal<f64>,
    pub size: RwSignal<Size>,
    pub num_window_tabs: Memo<usize>,
    pub window_maximized: RwSignal<bool>,
    pub window_tab_header_height: RwSignal<f64>,
    pub ime_allowed: RwSignal<bool>,
    pub cursor_blink_timer: RwSignal<TimerToken>,
    // the value to be update by cursor blinking
    pub hide_cursor: RwSignal<bool>,
    pub app_view_id: RwSignal<ViewId>,
}

/// `WindowData` is the application model for the top-level window.
///
/// Planck has one window: launching it again opens paths in that window
/// instead of a new one, so Untitled files are never left behind in another.
///
/// The window has a collection of "window tabs", one per workspace, plus its
/// size, position etc.
#[derive(Clone)]
pub struct WindowData {
    pub window_id: WindowId,
    pub scope: Scope,
    /// The set of tabs within the window. These tabs are high-level
    /// constructs for workspaces, in particular they are not **editor tabs**.
    pub window_tabs: RwSignal<im::Vector<(RwSignal<usize>, Rc<WindowTabData>)>>,
    pub num_window_tabs: Memo<usize>,
    /// The index of the active window tab.
    pub active: RwSignal<usize>,
    pub app_command: Listener<AppCommand>,
    pub position: RwSignal<Point>,
    pub root_view_id: RwSignal<ViewId>,
    pub window_scale: RwSignal<f64>,
    pub config: RwSignal<Arc<PlanckConfig>>,
    pub ime_enabled: RwSignal<bool>,
    pub common: Rc<WindowCommonData>,
}

impl WindowData {
    pub fn new(
        window_id: WindowId,
        app_view_id: RwSignal<ViewId>,
        info: WindowInfo,
        window_scale: RwSignal<f64>,
        app_command: Listener<AppCommand>,
    ) -> Self {
        let cx = Scope::new();
        let config = PlanckConfig::load(&PlanckWorkspace::default());
        let config = cx.create_rw_signal(Arc::new(config));
        let root_view_id = cx.create_rw_signal(ViewId::new());

        let window_tabs = cx.create_rw_signal(im::Vector::new());
        let num_window_tabs =
            cx.create_memo(move |_| window_tabs.with(|tabs| tabs.len()));
        let tabs = info.tabs.deduped();
        let active = tabs.active_tab;
        let window_command = Listener::new_empty(cx);
        let ime_allowed = cx.create_rw_signal(false);
        let window_maximized = cx.create_rw_signal(false);
        let size = cx.create_rw_signal(Size::ZERO);
        let window_tab_header_height = cx.create_rw_signal(0.0);
        let cursor_blink_timer = cx.create_rw_signal(TimerToken::INVALID);
        let hide_cursor = cx.create_rw_signal(false);

        let common = Rc::new(WindowCommonData {
            window_command,
            window_scale,
            size,
            num_window_tabs,
            window_maximized,
            window_tab_header_height,
            ime_allowed,
            cursor_blink_timer,
            hide_cursor,
            app_view_id,
        });

        for w in tabs.workspaces {
            let window_tab =
                Rc::new(WindowTabData::new(cx, Arc::new(w), common.clone()));
            window_tabs.update(|window_tabs| {
                window_tabs.push_back((cx.create_rw_signal(0), window_tab));
            });
        }

        if window_tabs.with_untracked(|window_tabs| window_tabs.is_empty()) {
            let window_tab = Rc::new(WindowTabData::new(
                cx,
                Arc::new(PlanckWorkspace::default()),
                common.clone(),
            ));
            window_tabs.update(|window_tabs| {
                window_tabs.push_back((cx.create_rw_signal(0), window_tab));
            });
        }

        let active = cx.create_rw_signal(active);
        let position = cx.create_rw_signal(info.pos);

        let window_data = Self {
            window_id,
            scope: cx,
            window_tabs,
            num_window_tabs,
            active,
            position,
            root_view_id,
            window_scale,
            app_command,
            config,
            ime_enabled: cx.create_rw_signal(false),
            common,
        };

        {
            let window_data = window_data.clone();
            window_data.common.window_command.listen(move |cmd| {
                window_data.run_window_command(cmd);
            });
        }

        {
            cx.create_effect(move |_| {
                let active = active.get();
                let tab = window_tabs
                    .with(|tabs| tabs.get(active).map(|(_, tab)| tab.clone()));
                if let Some(tab) = tab {
                    tab.common
                        .internal_command
                        .send(InternalCommand::ResetBlinkCursor);
                }
            })
        }

        window_data
    }

    pub fn reload_config(&self) {
        let config = PlanckConfig::load(&PlanckWorkspace::default());
        self.config.set(Arc::new(config));
        let window_tabs = self.window_tabs.get_untracked();
        for (_, window_tab) in window_tabs {
            window_tab.reload_config();
        }
    }

    pub fn run_window_command(&self, cmd: WindowCommand) {
        match cmd {
            WindowCommand::SetWorkspace { workspace } => {
                let db: Arc<PlanckDb> = use_context().unwrap();
                if let Err(err) = db.update_recent_workspace(&workspace) {
                    tracing::error!("{:?}", err);
                }

                let active = self.active_index();
                // Tabs that belong to no folder move to the new workspace, instead
                // of staying out of sight in the saved session of the one replaced:
                // Untitled files always, loose files when a first folder opens.
                let carry_files = self.window_tabs.with_untracked(|tabs| {
                    tabs.get(active)
                        .is_some_and(|(_, tab)| tab.workspace.is_files_only())
                }) && !workspace.is_files_only();
                let carried = self.take_editors(active, |editor| {
                    editor.is_untitled_with_text()
                        || (carry_files && editor.content.is_file())
                });

                // Already open in another tab: go there instead of opening it twice.
                if let Some(index) =
                    self.tab_index(&workspace).filter(|index| *index != active)
                {
                    self.remove_tab(active);
                    self.active
                        .set(if index > active { index - 1 } else { index });
                    if let Some(tab) = self.active_window_tab() {
                        tab.main_split.open_carried(carried);
                    }
                } else {
                    let window_tab = Rc::new(WindowTabData::with_tabs(
                        self.scope,
                        Arc::new(workspace),
                        self.common.clone(),
                        carried,
                    ));
                    self.window_tabs.update(|window_tabs| {
                        if window_tabs.is_empty() {
                            window_tabs.push_back((
                                self.scope.create_rw_signal(0),
                                window_tab,
                            ));
                        } else {
                            let (_, old_window_tab) = window_tabs.set(
                                active,
                                (self.scope.create_rw_signal(0), window_tab),
                            );
                            old_window_tab.proxy.shutdown();
                        }
                    })
                }
            }
            WindowCommand::NewWorkspaceTab { workspace, end } => {
                let db: Arc<PlanckDb> = use_context().unwrap();
                if let Err(err) = db.update_recent_workspace(&workspace) {
                    tracing::error!("{:?}", err);
                }

                // One tab per workspace: go to the open one. The files-only tab
                // is where new files live, so it starts one.
                if let Some(index) = self.tab_index(&workspace) {
                    self.active.set(index);
                    if workspace.is_files_only()
                        && let Some(tab) = self.active_window_tab()
                    {
                        tab.main_split.new_file();
                    }
                } else {
                    let window_tab = Rc::new(WindowTabData::new(
                        self.scope,
                        Arc::new(workspace),
                        self.common.clone(),
                    ));
                    let active = self.active.get_untracked();
                    let active = self
                        .window_tabs
                        .try_update(|tabs| {
                            if end || tabs.is_empty() {
                                tabs.push_back((
                                    self.scope.create_rw_signal(0),
                                    window_tab,
                                ));
                                tabs.len() - 1
                            } else {
                                let index = tabs.len().min(active + 1);
                                tabs.insert(
                                    index,
                                    (self.scope.create_rw_signal(0), window_tab),
                                );
                                index
                            }
                        })
                        .unwrap();
                    self.active.set(active);
                }
            }
            WindowCommand::CloseWorkspaceTab { index } => {
                let index = index.unwrap_or_else(|| self.active.get_untracked());
                let tabs_len = self.window_tabs.with_untracked(|tabs| tabs.len());
                if tabs_len >= 2 && index < tabs_len {
                    // Untitled files move to the tab shown next, instead of
                    // closing out of sight with this one.
                    let carried =
                        self.take_editors(index, EditorInfo::is_untitled_with_text);
                    self.remove_tab(index);
                    if let Some(tab) = self.active_window_tab() {
                        tab.main_split.open_carried(carried);
                    }
                }
            }
            WindowCommand::NextWorkspaceTab => {
                let active = self.active.get_untracked();
                let tabs_len = self.window_tabs.with_untracked(|tabs| tabs.len());
                if tabs_len > 1 {
                    let active = if active >= tabs_len - 1 {
                        0
                    } else {
                        active + 1
                    };
                    self.active.set(active);
                }
            }
            WindowCommand::PreviousWorkspaceTab => {
                let active = self.active.get_untracked();
                let tabs_len = self.window_tabs.with_untracked(|tabs| tabs.len());
                if tabs_len > 1 {
                    let active = if active == 0 {
                        tabs_len - 1
                    } else {
                        active - 1
                    };
                    self.active.set(active);
                }
            }
            WindowCommand::CloseWindow => {
                self.app_command
                    .send(AppCommand::CloseWindow(self.window_id));
            }
        }
        self.app_command.send(AppCommand::SaveApp);
    }

    pub fn key_down<'a>(&self, event: impl Into<EventRef<'a>> + Copy) -> bool {
        let active = self.active.get_untracked();
        let window_tab = self.window_tabs.with_untracked(|window_tabs| {
            window_tabs
                .get(active)
                .or_else(|| window_tabs.last())
                .cloned()
        });
        if let Some((_, window_tab)) = window_tab {
            window_tab.key_down(event)
        } else {
            false
        }
    }

    pub fn info(&self) -> WindowInfo {
        let workspaces: Vec<PlanckWorkspace> = self
            .window_tabs
            .get_untracked()
            .iter()
            .map(|(_, t)| (*t.workspace).clone())
            .collect();
        WindowInfo {
            size: self.common.size.get_untracked(),
            pos: self.position.get_untracked(),
            maximised: false,
            tabs: TabsInfo {
                active_tab: self.active.get_untracked(),
                workspaces,
            },
        }
    }

    /// The index of the active tab, within bounds.
    fn active_index(&self) -> usize {
        let tabs_len = self.window_tabs.with_untracked(|tabs| tabs.len());
        self.active.get_untracked().min(tabs_len.saturating_sub(1))
    }

    /// The index of the tab open on `workspace`'s folder or space.
    fn tab_index(&self, workspace: &PlanckWorkspace) -> Option<usize> {
        self.window_tabs.with_untracked(|tabs| {
            tabs.iter()
                .position(|(_, tab)| tab.workspace.is_same(workspace))
        })
    }

    /// Takes the editors that `take` picks out of the tab at `index`, and saves
    /// the session it's left with so they don't come back when it reopens.
    fn take_editors(
        &self,
        index: usize,
        take: impl Fn(&EditorInfo) -> bool,
    ) -> CarriedTabs {
        let mut carried = CarriedTabs::default();
        self.window_tabs.with_untracked(|tabs| {
            let Some((_, tab)) = tabs.get(index) else {
                return;
            };
            let mut info = tab.workspace_info();
            info.split.take_editors(&take, &mut carried);
            let db: Arc<PlanckDb> = use_context().unwrap();
            if let Err(err) = db.insert_window_tab_info(tab, &info) {
                tracing::error!("{:?}", err);
            }
        });
        carried
    }

    /// Removes the tab at `index` and stops its proxy. The active tab stays
    /// active, or the one before it when it was the one removed and the last.
    fn remove_tab(&self, index: usize) {
        let active = self.active.get_untracked();
        let removed = self
            .window_tabs
            .try_update(|tabs| (index < tabs.len()).then(|| tabs.remove(index).1))
            .flatten();
        if let Some(tab) = removed {
            tab.proxy.shutdown();
        }

        let tabs_len = self.window_tabs.with_untracked(|tabs| tabs.len());
        if active > index && active > 0 {
            self.active.set(active - 1);
        } else if active >= tabs_len.saturating_sub(1) {
            self.active.set(tabs_len.saturating_sub(1));
        }
    }

    pub fn active_window_tab(&self) -> Option<Rc<WindowTabData>> {
        let window_tabs = self.window_tabs.get_untracked();
        let active = self
            .active
            .get_untracked()
            .min(window_tabs.len().saturating_sub(1));
        window_tabs.get(active).map(|(_, tab)| tab.clone())
    }

    pub fn move_tab(&self, from_index: usize, to_index: usize) {
        if from_index == to_index {
            return;
        }

        let to_index = if from_index < to_index {
            to_index - 1
        } else {
            to_index
        };
        self.window_tabs.update(|tabs| {
            let tab = tabs.remove(from_index);
            tabs.insert(to_index, tab);
        });
        self.active.set(to_index);
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::TabsInfo;
    use crate::workspace::PlanckWorkspace;

    fn folder(path: &str) -> PlanckWorkspace {
        PlanckWorkspace {
            path: Some(PathBuf::from(path)),
            ..Default::default()
        }
    }

    fn names(tabs: &TabsInfo) -> Vec<String> {
        tabs.workspaces.iter().map(|w| w.to_string()).collect()
    }

    #[test]
    fn add_returns_the_tab_already_open_on_a_workspace() {
        let mut tabs = TabsInfo {
            active_tab: 0,
            workspaces: vec![PlanckWorkspace::default(), folder("/a")],
        };
        // Opened at another time, so last_open differs: still the same workspace.
        let mut a = folder("/a");
        a.last_open = 42;
        assert_eq!(tabs.add(a), 1);
        assert_eq!(tabs.add(PlanckWorkspace::default()), 0);
        assert_eq!(tabs.add(folder("/b")), 2);
        assert_eq!(names(&tabs), ["", "/a", "/b"]);
    }

    #[test]
    fn deduped_keeps_the_active_workspace_active() {
        let tabs = TabsInfo {
            active_tab: 3,
            workspaces: vec![
                folder("/a"),
                PlanckWorkspace::default(),
                folder("/a"),
                folder("/b"),
                PlanckWorkspace::default(),
            ],
        }
        .deduped();
        assert_eq!(names(&tabs), ["/a", "", "/b"]);
        assert_eq!(tabs.active_tab, 2);

        // Active on a duplicate: the first tab of that workspace.
        let tabs = TabsInfo {
            active_tab: 2,
            workspaces: vec![folder("/a"), folder("/b"), folder("/a")],
        }
        .deduped();
        assert_eq!(tabs.active_tab, 0);
    }
}
