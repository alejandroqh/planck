use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
};

use lapce_xi_rope::{Rope, RopeDelta};
use lsp_types::{
    DidOpenTextDocumentParams, MessageType, SemanticTokens, ShowMessageParams,
    TextDocumentIdentifier, TextDocumentItem, VersionedTextDocumentIdentifier,
    notification::{DidOpenTextDocument, Notification},
};
use parking_lot::Mutex;
use planck_rpc::{
    RpcError,
    core::LogLevel,
    plugin::{LspServerConfig, LspServers, PluginId},
    proxy::ProxyResponse,
    style::LineStyle,
};
use serde_json::Value;

use super::{
    PluginCatalogNotification, PluginCatalogRpcHandler,
    lsp::LspClient,
    psp::{CloneableCallback, PluginServerRpc, PluginServerRpcHandler, RpcCallback},
};

/// Owns the running language servers and routes requests to them.
pub struct PluginCatalog {
    workspace: Option<PathBuf>,
    plugin_rpc: PluginCatalogRpcHandler,
    plugins: HashMap<PluginId, PluginServerRpcHandler>,
    /// Language servers from the `[lsp.*]` settings.
    lsp_servers: LspServers,
    /// Servers already started (or attempted) in this session.
    started: HashSet<String>,
    /// Language id of every open file.
    open_files: HashMap<PathBuf, String>,
}

impl PluginCatalog {
    pub fn new(
        workspace: Option<PathBuf>,
        lsp_servers: LspServers,
        plugin_rpc: PluginCatalogRpcHandler,
    ) -> Self {
        Self {
            workspace,
            plugin_rpc,
            plugins: HashMap::new(),
            lsp_servers,
            started: HashSet::new(),
            open_files: HashMap::new(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn handle_server_request(
        &mut self,
        plugin_id: Option<PluginId>,
        request_sent: Option<Arc<AtomicUsize>>,
        method: Cow<'static, str>,
        params: Value,
        language_id: Option<String>,
        path: Option<PathBuf>,
        check: bool,
        f: Box<dyn CloneableCallback<Value, RpcError>>,
    ) {
        if let Some(plugin_id) = plugin_id {
            if let Some(plugin) = self.plugins.get(&plugin_id) {
                plugin.server_request_async(
                    method,
                    params,
                    language_id,
                    path,
                    check,
                    move |result| {
                        f(plugin_id, result);
                    },
                );
            } else {
                f(
                    plugin_id,
                    Err(RpcError {
                        code: 0,
                        message: "language server doesn't exist".to_string(),
                    }),
                );
            }
            return;
        }

        if let Some(request_sent) = request_sent {
            // With no servers running the callback would never be called, so
            // answer directly with an error.
            if self.plugins.is_empty() {
                request_sent.fetch_add(1, Ordering::Relaxed);
                f(
                    PluginId(0),
                    Err(RpcError {
                        code: 0,
                        message: "no language server is running".to_string(),
                    }),
                );
                return;
            } else {
                request_sent.fetch_add(self.plugins.len(), Ordering::Relaxed);
            }
        }
        for (plugin_id, plugin) in self.plugins.iter() {
            let f = dyn_clone::clone_box(&*f);
            let plugin_id = *plugin_id;
            plugin.server_request_async(
                method.clone(),
                params.clone(),
                language_id.clone(),
                path.clone(),
                check,
                move |result| {
                    f(plugin_id, result);
                },
            );
        }
    }

    pub fn handle_server_notification(
        &mut self,
        plugin_id: Option<PluginId>,
        method: impl Into<Cow<'static, str>>,
        params: Value,
        language_id: Option<String>,
        path: Option<PathBuf>,
        check: bool,
    ) {
        if let Some(plugin_id) = plugin_id {
            if let Some(plugin) = self.plugins.get(&plugin_id) {
                plugin.server_notification(method, params, language_id, path, check);
            }
            return;
        }

        let method = method.into();
        for plugin in self.plugins.values() {
            plugin.server_notification(
                method.clone(),
                params.clone(),
                language_id.clone(),
                path.clone(),
                check,
            );
        }
    }

    /// Start every enabled, not yet started server that handles `language_id`.
    fn start_servers_for_language(&mut self, language_id: &str) {
        let to_start: Vec<(String, LspServerConfig)> = self
            .lsp_servers
            .iter()
            .filter(|(name, config)| {
                !config.disabled
                    && !self.started.contains(*name)
                    && config.languages.iter().any(|l| l == language_id)
            })
            .map(|(name, config)| (name.clone(), config.clone()))
            .collect();

        for (name, config) in to_start {
            self.started.insert(name.clone());
            if config.command.is_empty() {
                self.plugin_rpc.core_rpc.log(
                    LogLevel::Warn,
                    format!("language server {name} has no command configured"),
                    None,
                );
                continue;
            }
            let workspace = self.workspace.clone();
            let plugin_rpc = self.plugin_rpc.clone();
            thread::spawn(move || {
                let core_rpc = plugin_rpc.core_rpc.clone();
                let command = config.command.clone();
                if let Err(err) =
                    LspClient::start(plugin_rpc, workspace, name.clone(), config)
                {
                    let not_found = err
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound);
                    if not_found {
                        // A default server that isn't installed: not an error.
                        core_rpc.log(
                            LogLevel::Info,
                            format!(
                                "language server {name} not started: `{command}` not found"
                            ),
                            None,
                        );
                    } else {
                        core_rpc.show_message(
                            format!("Language server: {name}"),
                            ShowMessageParams {
                                typ: MessageType::ERROR,
                                message: format!(
                                    "Failed to start `{command}`: {err}"
                                ),
                            },
                        );
                    }
                }
            });
        }
    }

    pub fn handle_did_open_text_document(&mut self, document: TextDocumentItem) {
        match document.uri.to_file_path() {
            Ok(path) => {
                self.open_files.insert(path, document.language_id.clone());
            }
            Err(err) => {
                tracing::error!("{:?}", err);
            }
        }

        self.start_servers_for_language(&document.language_id);

        let path = document.uri.to_file_path().ok();
        for plugin in self.plugins.values() {
            plugin.server_notification(
                DidOpenTextDocument::METHOD,
                DidOpenTextDocumentParams {
                    text_document: document.clone(),
                },
                Some(document.language_id.clone()),
                path.clone(),
                true,
            );
        }
    }

    pub fn handle_did_save_text_document(
        &mut self,
        language_id: String,
        path: PathBuf,
        text_document: TextDocumentIdentifier,
        text: Rope,
    ) {
        for plugin in self.plugins.values() {
            plugin.handle_rpc(PluginServerRpc::DidSaveTextDocument {
                language_id: language_id.clone(),
                path: path.clone(),
                text_document: text_document.clone(),
                text: text.clone(),
            });
        }
    }

    pub fn handle_did_change_text_document(
        &mut self,
        language_id: String,
        document: VersionedTextDocumentIdentifier,
        delta: RopeDelta,
        text: Rope,
        new_text: Rope,
    ) {
        let change = Arc::new(Mutex::new((None, None)));
        for plugin in self.plugins.values() {
            plugin.handle_rpc(PluginServerRpc::DidChangeTextDocument {
                language_id: language_id.clone(),
                document: document.clone(),
                delta: delta.clone(),
                text: text.clone(),
                new_text: new_text.clone(),
                change: change.clone(),
            });
        }
    }

    pub fn format_semantic_tokens(
        &self,
        plugin_id: PluginId,
        tokens: SemanticTokens,
        text: Rope,
        f: Box<dyn RpcCallback<Vec<LineStyle>, RpcError>>,
    ) {
        if let Some(plugin) = self.plugins.get(&plugin_id) {
            plugin.handle_rpc(PluginServerRpc::FormatSemanticTokens {
                tokens,
                text,
                f,
            });
        } else {
            f.call(Err(RpcError {
                code: 0,
                message: "language server doesn't exist".to_string(),
            }));
        }
    }

    pub fn handle_notification(&mut self, notification: PluginCatalogNotification) {
        use PluginCatalogNotification::*;
        match notification {
            UpdateLspServers(servers) => {
                tracing::debug!("UpdateLspServers {:?}", servers);
                self.lsp_servers = servers;
                // Start servers that were added for files that are already open.
                let languages: HashSet<String> =
                    self.open_files.values().cloned().collect();
                for language_id in languages {
                    self.start_servers_for_language(&language_id);
                }
            }
            PluginServerLoaded(plugin) => {
                match self.plugin_rpc.proxy_rpc.get_open_files_content() {
                    Ok(ProxyResponse::GetOpenFilesContentResponse { items }) => {
                        for item in items {
                            let language_id = Some(item.language_id.clone());
                            let path = item.uri.to_file_path().ok();
                            plugin.server_notification(
                                DidOpenTextDocument::METHOD,
                                DidOpenTextDocumentParams {
                                    text_document: item,
                                },
                                language_id,
                                path,
                                true,
                            );
                        }
                    }
                    Ok(_) => {}
                    Err(err) => {
                        tracing::error!("{:?}", err);
                    }
                }
                self.plugins.insert(plugin.plugin_id, plugin);
            }
            Shutdown => {
                for plugin in self.plugins.values() {
                    plugin.shutdown();
                }
            }
        }
    }
}
