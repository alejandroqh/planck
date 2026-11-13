use std::{
    process::Command,
    sync::{Arc, mpsc::Sender},
};

use floem::{ext_event::create_signal_from_channel, reactive::ReadSignal};
use planck_proxy::dispatch::Dispatcher;
use planck_rpc::{
    core::{CoreHandler, CoreNotification, CoreRpcHandler},
    plugin::LspServers,
    proxy::{ProxyRpcHandler, ProxyStatus},
    terminal::TermId,
};

use crate::{terminal::event::TermEvent, workspace::PlanckWorkspace};

pub struct Proxy {
    pub tx: Sender<CoreNotification>,
    pub term_tx: Sender<(TermId, TermEvent)>,
}

#[derive(Clone)]
pub struct ProxyData {
    pub proxy_rpc: ProxyRpcHandler,
    pub core_rpc: CoreRpcHandler,
    pub notification: ReadSignal<Option<CoreNotification>>,
}

impl ProxyData {
    pub fn shutdown(&self) {
        self.proxy_rpc.shutdown();
        self.core_rpc.shutdown();
    }
}

pub fn new_proxy(
    workspace: Arc<PlanckWorkspace>,
    lsp_servers: LspServers,
    term_tx: Sender<(TermId, TermEvent)>,
) -> ProxyData {
    let proxy_rpc = ProxyRpcHandler::new();
    let core_rpc = CoreRpcHandler::new();

    {
        let core_rpc = core_rpc.clone();
        let proxy_rpc = proxy_rpc.clone();
        std::thread::Builder::new()
            .name("ProxyRpcHandler".to_owned())
            .spawn(move || {
                core_rpc.notification(CoreNotification::ProxyStatus {
                    status: ProxyStatus::Connecting,
                });
                proxy_rpc.initialize(workspace.folder_paths(), lsp_servers, 1, 1);

                let mut dispatcher =
                    Dispatcher::new(core_rpc.clone(), proxy_rpc.clone());
                let proxy_rpc = dispatcher.proxy_rpc.clone();
                proxy_rpc.mainloop(&mut dispatcher);
                core_rpc.notification(CoreNotification::ProxyStatus {
                    status: ProxyStatus::Disconnected,
                });
            })
            .unwrap();
    }

    let (tx, rx) = std::sync::mpsc::channel();
    {
        let core_rpc = core_rpc.clone();
        std::thread::Builder::new()
            .name("CoreRpcHandler".to_owned())
            .spawn(move || {
                let mut proxy = Proxy { tx, term_tx };
                core_rpc.mainloop(&mut proxy);
                core_rpc.notification(CoreNotification::ProxyStatus {
                    status: ProxyStatus::Disconnected,
                });
            })
            .unwrap()
    };

    let notification = create_signal_from_channel(rx);

    ProxyData {
        proxy_rpc,
        core_rpc,
        notification,
    }
}

impl CoreHandler for Proxy {
    fn handle_notification(&mut self, rpc: planck_rpc::core::CoreNotification) {
        // Logs only go to tracing, so handle them here instead of waking the UI thread.
        match &rpc {
            CoreNotification::Log {
                level,
                message,
                target,
            } => {
                use planck_rpc::core::LogLevel;
                use tracing_log::log::{Level, log};

                let target = target.clone().unwrap_or(String::from("unknown"));

                match level {
                    LogLevel::Trace => {
                        log!(target: &target, Level::Trace, "{}", message);
                    }
                    LogLevel::Debug => {
                        log!(target: &target, Level::Debug, "{}", message);
                    }
                    LogLevel::Info => {
                        log!(target: &target, Level::Info, "{}", message);
                    }
                    LogLevel::Warn => {
                        log!(target: &target, Level::Warn, "{}", message);
                    }
                    LogLevel::Error => {
                        log!(target: &target, Level::Error, "{}", message);
                    }
                }
                return;
            }
            CoreNotification::LogMessage { message, target } => {
                use lsp_types::MessageType;
                use tracing_log::log::{Level, log};
                match message.typ {
                    MessageType::ERROR => {
                        log!(target: target, Level::Error, "{}", message.message)
                    }
                    MessageType::WARNING => {
                        log!(target: target, Level::Warn, "{}", message.message)
                    }
                    MessageType::INFO => {
                        log!(target: target, Level::Info, "{}", message.message)
                    }
                    MessageType::DEBUG => {
                        log!(target: target, Level::Debug, "{}", message.message)
                    }
                    MessageType::LOG => {
                        log!(target: target, Level::Debug, "{}", message.message)
                    }
                    _ => {}
                }
                return;
            }
            _ => {}
        }
        let rpc = match rpc {
            CoreNotification::UpdateTerminal { term_id, content } => {
                if let Err(err) = self
                    .term_tx
                    .send((term_id, TermEvent::UpdateContent(content)))
                {
                    tracing::error!("{:?}", err);
                }
                return;
            }
            rpc => rpc,
        };
        if let Err(err) = self.tx.send(rpc) {
            tracing::error!("{:?}", err);
        }
    }

    fn handle_request(
        &mut self,
        _id: planck_rpc::RequestId,
        _rpc: planck_rpc::core::CoreRequest,
    ) {
    }
}

pub fn new_command(program: &str) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(program);
    #[cfg(target_os = "windows")]
    use std::os::windows::process::CommandExt;
    #[cfg(target_os = "windows")]
    cmd.creation_flags(0x08000000);
    cmd
}
