use std::{rc::Rc, sync::Arc};

use floem::reactive::{RwSignal, Scope, SignalGet, SignalUpdate, SignalWith};
use planck_core::mode::Mode;
use planck_rpc::{
    run::RunConfig,
    terminal::{TermId, TerminalProfile},
};

use super::{data::TerminalData, tab::TerminalTabData};
use crate::{
    id::TerminalTabId,
    keypress::{EventRef, KeyPressData, KeyPressFocus, KeyPressHandle},
    main_split::MainSplitData,
    panel::kind::PanelKind,
    run::{RunConfigs, RunProcess},
    window_tab::{CommonData, Focus},
    workspace::PlanckWorkspace,
};

pub struct TerminalTabInfo {
    pub active: usize,
    pub tabs: im::Vector<(RwSignal<usize>, TerminalTabData)>,
}

#[derive(Clone)]
pub struct TerminalPanelData {
    pub cx: Scope,
    pub workspace: Arc<PlanckWorkspace>,
    pub tab_info: RwSignal<TerminalTabInfo>,
    pub common: Rc<CommonData>,
    pub main_split: MainSplitData,
}

impl TerminalPanelData {
    pub fn new(
        workspace: Arc<PlanckWorkspace>,
        profile: Option<TerminalProfile>,
        common: Rc<CommonData>,
        main_split: MainSplitData,
    ) -> Self {
        let terminal_tab =
            TerminalTabData::new(workspace.clone(), profile, common.clone());

        let cx = common.scope;

        let tabs =
            im::vector![(terminal_tab.scope.create_rw_signal(0), terminal_tab)];
        let tab_info = TerminalTabInfo { active: 0, tabs };
        let tab_info = cx.create_rw_signal(tab_info);

        Self {
            cx,
            workspace,
            tab_info,
            common,
            main_split,
        }
    }

    pub fn active_tab(&self, tracked: bool) -> Option<TerminalTabData> {
        if tracked {
            self.tab_info.with(|info| {
                info.tabs
                    .get(info.active)
                    .or_else(|| info.tabs.last())
                    .cloned()
                    .map(|(_, tab)| tab)
            })
        } else {
            self.tab_info.with_untracked(|info| {
                info.tabs
                    .get(info.active)
                    .or_else(|| info.tabs.last())
                    .cloned()
                    .map(|(_, tab)| tab)
            })
        }
    }

    pub fn key_down<'a>(
        &self,
        event: impl Into<EventRef<'a>> + Copy,
        keypress: &KeyPressData,
    ) -> Option<KeyPressHandle> {
        if self.tab_info.with_untracked(|info| info.tabs.is_empty()) {
            self.new_tab(None);
        }

        let tab = self.active_tab(false);
        let terminal = tab.and_then(|tab| tab.active_terminal(false));
        if let Some(terminal) = terminal {
            let handle = keypress.key_down(event, &terminal);
            let mode = terminal.get_mode();

            if !handle.handled
                && mode == Mode::Terminal
                && let EventRef::Keyboard(key_event) = event.into()
                && terminal.send_keypress(key_event)
            {
                return Some(KeyPressHandle {
                    handled: true,
                    keymatch: handle.keymatch,
                    keypress: handle.keypress,
                });
            }
            Some(handle)
        } else {
            None
        }
    }

    pub fn new_tab(&self, profile: Option<TerminalProfile>) {
        self.new_tab_run_debug(None, profile);
    }

    /// Create a new terminal tab with the given run debug process.  
    /// Errors if expanding out the run debug process failed.
    pub fn new_tab_run_debug(
        &self,
        run_debug: Option<RunProcess>,
        profile: Option<TerminalProfile>,
    ) -> TerminalTabData {
        let terminal_tab = TerminalTabData::new_run_debug(
            self.workspace.clone(),
            run_debug,
            profile,
            self.common.clone(),
        );

        self.tab_info.update(|info| {
            info.tabs.insert(
                if info.tabs.is_empty() {
                    0
                } else {
                    (info.active + 1).min(info.tabs.len())
                },
                (terminal_tab.scope.create_rw_signal(0), terminal_tab.clone()),
            );
            let new_active = (info.active + 1).min(info.tabs.len() - 1);
            info.active = new_active;
        });

        terminal_tab
    }

    pub fn next_tab(&self) {
        self.tab_info.update(|info| {
            if info.active >= info.tabs.len().saturating_sub(1) {
                info.active = 0;
            } else {
                info.active += 1;
            }
        });
    }

    pub fn previous_tab(&self) {
        self.tab_info.update(|info| {
            if info.active == 0 {
                info.active = info.tabs.len().saturating_sub(1);
            } else {
                info.active -= 1;
            }
        });
    }

    pub fn close_tab(&self, terminal_tab_id: Option<TerminalTabId>) {
        if let Some(close_tab) = self
            .tab_info
            .try_update(|info| {
                let mut close_tab = None;
                if let Some(terminal_tab_id) = terminal_tab_id {
                    if let Some(index) =
                        info.tabs.iter().enumerate().find_map(|(index, (_, t))| {
                            if t.terminal_tab_id == terminal_tab_id {
                                Some(index)
                            } else {
                                None
                            }
                        })
                    {
                        close_tab = Some(
                            info.tabs.remove(index).1.terminals.get_untracked(),
                        );
                    }
                } else {
                    let active = info.active.min(info.tabs.len().saturating_sub(1));
                    if !info.tabs.is_empty() {
                        info.tabs.remove(active);
                    }
                }
                let new_active = info.active.min(info.tabs.len().saturating_sub(1));
                info.active = new_active;
                close_tab
            })
            .flatten()
        {
            for (_, data) in close_tab {
                data.stop();
            }
        }
    }

    pub fn set_title(&self, term_id: &TermId, title: &str) {
        if let Some(t) = self.get_terminal(term_id) {
            t.title.set(title.to_string());
        }
    }

    pub fn get_terminal(&self, term_id: &TermId) -> Option<TerminalData> {
        self.tab_info.with_untracked(|info| {
            for (_, tab) in &info.tabs {
                let terminal = tab.terminals.with_untracked(|terminals| {
                    terminals
                        .iter()
                        .find(|(_, t)| &t.term_id == term_id)
                        .cloned()
                });
                if let Some(terminal) = terminal {
                    return Some(terminal.1);
                }
            }
            None
        })
    }

    fn get_terminal_in_tab(
        &self,
        term_id: &TermId,
    ) -> Option<(usize, TerminalTabData, usize, TerminalData)> {
        self.tab_info.with_untracked(|info| {
            for (tab_index, (_, tab)) in info.tabs.iter().enumerate() {
                let result = tab.terminals.with_untracked(|terminals| {
                    terminals
                        .iter()
                        .enumerate()
                        .find(|(_, (_, t))| &t.term_id == term_id)
                        .map(|(i, (_, terminal))| (i, terminal.clone()))
                });
                if let Some((index, terminal)) = result {
                    return Some((tab_index, tab.clone(), index, terminal));
                }
            }
            None
        })
    }

    pub fn split(&self, term_id: TermId) {
        if let Some((_, tab, index, _)) = self.get_terminal_in_tab(&term_id) {
            let terminal_data = TerminalData::new(
                tab.scope,
                self.workspace.clone(),
                None,
                self.common.clone(),
            );
            let i = terminal_data.scope.create_rw_signal(0);
            tab.terminals.update(|terminals| {
                terminals.insert(index + 1, (i, terminal_data));
            });
        }
    }

    pub fn split_next(&self, term_id: TermId) {
        if let Some((_, tab, index, _)) = self.get_terminal_in_tab(&term_id) {
            let max = tab.terminals.with_untracked(|t| t.len() - 1);
            let new_index = (index + 1).min(max);
            if new_index != index {
                tab.active.set(new_index);
            }
        }
    }

    pub fn split_previous(&self, term_id: TermId) {
        if let Some((_, tab, index, _)) = self.get_terminal_in_tab(&term_id) {
            let new_index = index.saturating_sub(1);
            if new_index != index {
                tab.active.set(new_index);
            }
        }
    }

    pub fn split_exchange(&self, term_id: TermId) {
        if let Some((_, tab, index, _)) = self.get_terminal_in_tab(&term_id) {
            let max = tab.terminals.with_untracked(|t| t.len() - 1);
            if index < max {
                tab.terminals.update(|terminals| {
                    terminals.swap(index, index + 1);
                });
            }
        }
    }

    pub fn close_terminal(&self, term_id: &TermId) {
        if let Some((_, tab, index, _)) = self.get_terminal_in_tab(term_id) {
            let active = tab.active.get_untracked();
            let len = tab
                .terminals
                .try_update(|terminals| {
                    terminals.remove(index);
                    terminals.len()
                })
                .unwrap();
            if len == 0 {
                self.close_tab(Some(tab.terminal_tab_id));
            } else {
                let new_active = active.min(len.saturating_sub(1));
                if new_active != active {
                    tab.active.set(new_active);
                }
            }
        }
    }

    pub fn launch_failed(&self, term_id: &TermId, error: &str) {
        if let Some(terminal) = self.get_terminal(term_id) {
            terminal.launch_error.set(Some(error.to_string()));
        }
    }

    pub fn terminal_stopped(&self, term_id: &TermId, exit_code: Option<i32>) {
        let Some(terminal) = self.get_terminal(term_id) else {
            return;
        };
        if terminal.run_debug.with_untracked(|r| r.is_none()) {
            self.close_terminal(term_id);
            return;
        }
        let was_prelaunch = terminal
            .run_debug
            .try_update(|run| {
                let run = run.as_mut()?;
                if run.is_prelaunch && run.config.prelaunch.is_some() {
                    run.is_prelaunch = false;
                    Some(true)
                } else {
                    run.stopped = true;
                    Some(false)
                }
            })
            .unwrap();
        if was_prelaunch == Some(true)
            && exit_code.unwrap_or(0) == 0
            && let Some(run) = terminal.run_debug.get_untracked()
        {
            terminal.new_process(Some(run));
        }
    }

    pub fn get_stopped_run_terminal(
        &self,
        config: &RunConfig,
    ) -> Option<TerminalData> {
        self.tab_info.with_untracked(|info| {
            info.tabs.iter().find_map(|(_, tab)| {
                tab.terminals.with_untracked(|terminals| {
                    terminals.iter().find_map(|(_, terminal)| {
                        terminal
                            .run_debug
                            .with_untracked(|run| {
                                run.as_ref().is_some_and(|run| {
                                    run.stopped && run.config.name == config.name
                                })
                            })
                            .then(|| terminal.clone())
                    })
                })
            })
        })
    }

    pub fn restart_run(&self, term_id: TermId) -> Option<()> {
        let (_, terminal_tab, index, terminal) =
            self.get_terminal_in_tab(&term_id)?;
        let mut run = terminal.run_debug.get_untracked()?;
        if run.config.config_source.from_palette()
            && let Some(new_config) = self.get_run_config_by_name(&run.config.name)
        {
            run.config = new_config;
        }
        self.common.proxy.terminal_close(term_id);
        run.stopped = false;
        run.is_prelaunch = true;
        let new_terminal = TerminalData::new_run_debug(
            terminal_tab.scope,
            self.workspace.clone(),
            Some(run),
            None,
            self.common.clone(),
        );
        let new_term_id = new_terminal.term_id;
        terminal_tab.terminals.update(|terminals| {
            terminals[index] =
                (new_terminal.scope.create_rw_signal(0), new_terminal);
        });
        self.focus_terminal(new_term_id);
        Some(())
    }

    pub fn stop_run(&self, term_id: TermId) -> Option<()> {
        let terminal = self.get_terminal(&term_id)?;
        terminal.run_debug.get_untracked()?;
        self.common.proxy.terminal_close(term_id);
        self.focus_terminal(term_id);
        Some(())
    }

    fn get_run_config_by_name(&self, name: &str) -> Option<RunConfig> {
        if let Some(workspace) = self.common.workspace.path.as_deref() {
            let run_toml = workspace.join(".planck").join("run.toml");
            let (doc, new_doc) = self.main_split.get_doc(run_toml.clone(), None);
            if !new_doc {
                let content = doc.buffer.with_untracked(|b| b.to_string());
                match toml::from_str::<RunConfigs>(&content) {
                    Ok(configs) => {
                        return configs.configs.into_iter().find(|x| x.name == name);
                    }
                    Err(err) => {
                        // todo show message window
                        tracing::error!("deser fail {:?}", err);
                    }
                }
            }
        }
        None
    }

    pub fn focus_terminal(&self, term_id: TermId) {
        if let Some((tab_index, terminal_tab, index, _terminal)) =
            self.get_terminal_in_tab(&term_id)
        {
            self.tab_info.update(|info| {
                info.active = tab_index;
            });
            terminal_tab.active.set(index);
            self.common.focus.set(Focus::Panel(PanelKind::Terminal));
        }
    }

    pub fn run_debug_process(&self, tracked: bool) -> Vec<(TermId, RunProcess)> {
        let mut processes = Vec::new();
        if tracked {
            self.tab_info.with(|info| {
                for (_, tab) in &info.tabs {
                    tab.terminals.with(|terminals| {
                        for (_, terminal) in terminals {
                            if let Some(run_debug) = terminal.run_debug.get() {
                                processes.push((terminal.term_id, run_debug));
                            }
                        }
                    })
                }
            });
        } else {
            self.tab_info.with_untracked(|info| {
                for (_, tab) in &info.tabs {
                    tab.terminals.with_untracked(|terminals| {
                        for (_, terminal) in terminals {
                            if let Some(run_debug) = terminal.run_debug.get() {
                                processes.push((terminal.term_id, run_debug));
                            }
                        }
                    })
                }
            });
        }
        processes.sort_by_key(|(_, process)| process.created);
        processes
    }
}
