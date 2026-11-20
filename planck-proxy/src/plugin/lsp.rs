#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;
use std::{
    io::{BufRead, BufReader, BufWriter, Write},
    path::{Path, PathBuf},
    process::{self, Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};

use anyhow::{Result, anyhow};
use jsonrpc_lite::{Id, Params};
use lapce_xi_rope::Rope;
use lsp_types::{
    notification::{Initialized, Notification},
    request::{Initialize, Request},
    *,
};
use parking_lot::Mutex;
use planck_core::meta;
use planck_rpc::{
    RpcError,
    plugin::{LspServerConfig, PluginId},
    style::LineStyle,
};
use serde_json::Value;

use super::{
    client_capabilities,
    psp::{
        PluginHandlerNotification, PluginHostHandler, PluginServerHandler,
        PluginServerRpcHandler, ResponseSender, RpcCallback,
        handle_plugin_server_message,
    },
};
use crate::{buffer::Buffer, plugin::PluginCatalogRpcHandler};

const HEADER_CONTENT_LENGTH: &str = "content-length";
const HEADER_CONTENT_TYPE: &str = "content-type";

pub enum LspRpc {
    Request {
        id: u64,
        method: String,
        params: Params,
    },
    Notification {
        method: String,
        params: Params,
    },
    Response {
        id: u64,
        result: Value,
    },
    Error {
        id: u64,
        error: RpcError,
    },
}

pub struct LspClient {
    plugin_rpc: PluginCatalogRpcHandler,
    server_rpc: PluginServerRpcHandler,
    process: Child,
    workspace: Option<PathBuf>,
    host: PluginHostHandler,
    options: Option<Value>,
    /// Set before Planck kills the server, so that exit isn't reported as a failure.
    stopping: Arc<AtomicBool>,
}

impl PluginServerHandler for LspClient {
    fn method_registered(&mut self, method: &str) -> bool {
        self.host.method_registered(method)
    }

    fn document_supported(
        &mut self,
        language_id: Option<&str>,
        path: Option<&Path>,
    ) -> bool {
        self.host.document_supported(language_id, path)
    }

    fn handle_handler_notification(
        &mut self,
        notification: PluginHandlerNotification,
    ) {
        use PluginHandlerNotification::*;
        match notification {
            Initialize => {
                self.initialize();
            }
            InitializeResult(result) => {
                self.host.server_capabilities = result.capabilities;
            }
            Shutdown => {
                self.shutdown();
            }
        }
    }

    fn handle_host_request(
        &mut self,
        id: Id,
        method: String,
        params: Params,
        resp: ResponseSender,
    ) {
        self.host.handle_request(id, method, params, resp);
    }

    fn handle_host_notification(
        &mut self,
        method: String,
        params: Params,
        from: String,
    ) {
        if let Err(err) = self.host.handle_notification(method, params, from) {
            tracing::error!("{:?}", err);
        }
    }

    fn handle_did_save_text_document(
        &self,
        language_id: String,
        path: PathBuf,
        text_document: TextDocumentIdentifier,
        text: lapce_xi_rope::Rope,
    ) {
        self.host.handle_did_save_text_document(
            language_id,
            path,
            text_document,
            text,
        );
    }

    fn handle_did_change_text_document(
        &mut self,
        language_id: String,
        document: lsp_types::VersionedTextDocumentIdentifier,
        delta: lapce_xi_rope::RopeDelta,
        text: lapce_xi_rope::Rope,
        new_text: lapce_xi_rope::Rope,
        change: Arc<
            Mutex<(
                Option<TextDocumentContentChangeEvent>,
                Option<TextDocumentContentChangeEvent>,
            )>,
        >,
    ) {
        self.host.handle_did_change_text_document(
            language_id,
            document,
            delta,
            text,
            new_text,
            change,
        );
    }

    fn format_semantic_tokens(
        &self,
        tokens: SemanticTokens,
        text: Rope,
        f: Box<dyn RpcCallback<Vec<LineStyle>, RpcError>>,
    ) {
        self.host.format_semantic_tokens(tokens, text, f);
    }
}

impl LspClient {
    fn new(
        plugin_rpc: PluginCatalogRpcHandler,
        workspace: Option<PathBuf>,
        name: String,
        config: LspServerConfig,
    ) -> Result<Self> {
        let server = config.command.clone();
        let document_selector: DocumentSelector = config
            .languages
            .iter()
            .map(|language| lsp_types::DocumentFilter {
                language: Some(language.clone()),
                scheme: None,
                pattern: None,
            })
            .collect();
        let options = config.initialization_options.clone();

        let mut process = Self::process(workspace.as_ref(), &config)?;
        let stdin = process.stdin.take().unwrap();
        let stdout = process.stdout.take().unwrap();
        let stderr = process.stderr.take().unwrap();

        let mut writer = Box::new(BufWriter::new(stdin));
        let (io_tx, io_rx) = crossbeam_channel::unbounded();
        let server_rpc =
            PluginServerRpcHandler::new(name.clone(), None, io_tx.clone());
        thread::spawn(move || {
            for msg in io_rx {
                if msg
                    .get_method()
                    .map(|x| x == lsp_types::request::Shutdown::METHOD)
                    .unwrap_or_default()
                {
                    break;
                }
                if let Ok(msg) = serde_json::to_string(&msg) {
                    tracing::debug!("write to lsp: {}", msg);
                    if let Err(err) =
                        write!(writer, "Content-Length: {}\r\n\r\n", msg.len())
                            .and_then(|_| writer.write_all(msg.as_bytes()))
                    {
                        tracing::error!("{:?}", err);
                    }
                    if let Err(err) = writer.flush() {
                        tracing::error!("{:?}", err);
                    }
                }
            }
        });

        let core_rpc = plugin_rpc.core_rpc.clone();
        let name_closure = name.clone();
        // Returns the last lines the server printed, to explain why it stopped.
        let stderr_thread = thread::spawn(move || {
            let name = name_closure;
            let mut tail = std::collections::VecDeque::new();
            for line in BufReader::new(stderr).lines() {
                let Ok(line) = line else { break };
                let line = line.trim_end().to_string();
                core_rpc.log(
                    planck_rpc::core::LogLevel::Trace,
                    line.clone(),
                    Some(format!("planck_proxy::plugin::lsp::{name}::stderr")),
                );
                if !line.is_empty() {
                    if tail.len() == 5 {
                        tail.pop_front();
                    }
                    tail.push_back(line);
                }
            }
            Vec::from(tail).join("\n")
        });

        let stopping = Arc::new(AtomicBool::new(false));
        let stopping_closure = stopping.clone();
        let local_server_rpc = server_rpc.clone();
        let core_rpc = plugin_rpc.core_rpc.clone();
        let name_closure = name.clone();
        thread::spawn(move || {
            let name = name_closure;
            let mut reader = Box::new(BufReader::new(stdout));
            loop {
                match read_message(&mut reader) {
                    Ok(message_str) => {
                        if !message_str.contains("$/progress") {
                            tracing::debug!("read from lsp: {}", message_str);
                        }
                        if let Some(resp) = handle_plugin_server_message(
                            &local_server_rpc,
                            &message_str,
                            &name,
                            &io_tx,
                        ) && let Err(err) = io_tx.send(resp)
                        {
                            tracing::error!("{:?}", err);
                        }
                    }
                    Err(_err) => {
                        core_rpc.log(
                            planck_rpc::core::LogLevel::Error,
                            format!("language server {name} ({server}) stopped"),
                            Some(format!(
                                "planck_proxy::plugin::lsp::{name}::stopped"
                            )),
                        );
                        if !stopping_closure.load(Ordering::Relaxed) {
                            let stderr = stderr_thread.join().unwrap_or_default();
                            core_rpc.show_message(
                                format!("Language server: {name}"),
                                ShowMessageParams {
                                    typ: MessageType::ERROR,
                                    message: stopped_message(&server, &stderr),
                                },
                            );
                        }
                        return;
                    }
                };
            }
        });

        let host = PluginHostHandler::new(
            name,
            document_selector,
            plugin_rpc.core_rpc.clone(),
            server_rpc.clone(),
            plugin_rpc.clone(),
        );

        Ok(Self {
            plugin_rpc,
            server_rpc,
            process,
            workspace,
            host,
            options,
            stopping,
        })
    }

    /// Spawn the language server `name` and run its message loop on a
    /// background thread.
    pub fn start(
        plugin_rpc: PluginCatalogRpcHandler,
        workspace: Option<PathBuf>,
        name: String,
        config: LspServerConfig,
    ) -> Result<PluginId> {
        let mut lsp = Self::new(plugin_rpc, workspace, name, config)?;
        let plugin_id = lsp.server_rpc.plugin_id;

        let rpc = lsp.server_rpc.clone();
        thread::spawn(move || {
            rpc.mainloop(&mut lsp);
        });
        Ok(plugin_id)
    }

    fn initialize(&mut self) {
        let root_uri = self
            .workspace
            .clone()
            .map(|p| Url::from_directory_path(p).unwrap());
        tracing::debug!("initialization_options {:?}", self.options);
        #[allow(deprecated)]
        let params = InitializeParams {
            process_id: Some(process::id()),
            root_uri: root_uri.clone(),
            initialization_options: self.options.clone(),
            capabilities: client_capabilities(),
            trace: Some(TraceValue::Verbose),
            workspace_folders: root_uri.map(|uri| {
                vec![WorkspaceFolder {
                    name: uri.as_str().to_string(),
                    uri,
                }]
            }),
            client_info: Some(ClientInfo {
                name: meta::NAME.to_owned(),
                version: Some(meta::VERSION.to_owned()),
            }),
            locale: None,
            root_path: None,
            work_done_progress_params: WorkDoneProgressParams::default(),
        };
        match self.server_rpc.server_request(
            Initialize::METHOD,
            params,
            None,
            None,
            false,
        ) {
            Ok(value) => {
                let result: InitializeResult = match serde_json::from_value(value) {
                    Ok(result) => result,
                    Err(err) => {
                        self.plugin_rpc.core_rpc.show_message(
                            format!("Language server: {}", self.server_rpc.server_name),
                            ShowMessageParams {
                                typ: MessageType::ERROR,
                                message: format!(
                                    "Planck couldn't read the server's capabilities, so it was stopped: {err}"
                                ),
                            },
                        );
                        self.server_rpc.shutdown();
                        self.shutdown();
                        return;
                    }
                };
                self.host.server_capabilities = result.capabilities;
                self.server_rpc.server_notification(
                    Initialized::METHOD,
                    InitializedParams {},
                    None,
                    None,
                    false,
                );
                if self
                    .plugin_rpc
                    .plugin_server_loaded(self.server_rpc.clone())
                    .is_err()
                {
                    self.server_rpc.shutdown();
                    self.shutdown();
                }
            }
            Err(err) => {
                tracing::error!("{:?}", err);
            }
        }
        //     move |result| {
        //         if let Ok(value) = result {
        //             let result: InitializeResult =
        //                 serde_json::from_value(value).unwrap();
        //             server_rpc.handle_rpc(PluginServerRpc::Handler(
        //                 PluginHandlerNotification::InitializeDone(result),
        //             ));
        //         }
        //     },
        // );
    }

    fn shutdown(&mut self) {
        self.stopping.store(true, Ordering::Relaxed);
        if let Err(err) = self.process.kill() {
            tracing::error!("{:?}", err);
        }
        if let Err(err) = self.process.wait() {
            tracing::error!("{:?}", err);
        }
    }

    fn process(
        workspace: Option<&PathBuf>,
        config: &LspServerConfig,
    ) -> Result<Child> {
        let mut process = Command::new(&config.command);
        if let Some(workspace) = workspace {
            process.current_dir(workspace);
        }

        process.args(&config.args).envs(&config.env);

        #[cfg(target_os = "windows")]
        let process = process.creation_flags(0x08000000);
        let child = process
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        Ok(child)
    }
}

pub struct DocumentFilter {
    /// The document must have this language id, if it exists
    pub language_id: Option<String>,
    /// The document's path must match this glob, if it exists
    pub pattern: Option<globset::GlobMatcher>,
    // TODO: URI Scheme from lsp-types document filter
}
impl DocumentFilter {
    /// Constructs a document filter from the LSP version
    /// This ignores any fields that are badly constructed
    pub(crate) fn from_lsp_filter_loose(
        filter: &lsp_types::DocumentFilter,
    ) -> DocumentFilter {
        DocumentFilter {
            language_id: filter.language.clone(),
            // TODO: clean this up
            pattern: filter
                .pattern
                .as_deref()
                .map(globset::Glob::new)
                .and_then(Result::ok)
                .map(|x| globset::Glob::compile_matcher(&x)),
        }
    }
}

pub enum LspHeader {
    ContentType,
    ContentLength(usize),
}

fn parse_header(s: &str) -> Result<LspHeader> {
    let split: Vec<String> =
        s.splitn(2, ": ").map(|s| s.trim().to_lowercase()).collect();
    if split.len() != 2 {
        return Err(anyhow!("Malformed"));
    };
    match split[0].as_ref() {
        HEADER_CONTENT_TYPE => Ok(LspHeader::ContentType),
        HEADER_CONTENT_LENGTH => {
            Ok(LspHeader::ContentLength(split[1].parse::<usize>()?))
        }
        _ => Err(anyhow!("Unknown parse error occurred")),
    }
}

pub fn read_message<T: BufRead>(reader: &mut T) -> Result<String> {
    let mut buffer = String::new();
    let mut content_length: Option<usize> = None;

    loop {
        buffer.clear();
        let _ = reader.read_line(&mut buffer)?;
        // eprin
        match &buffer {
            s if s.trim().is_empty() => break,
            s => {
                match parse_header(s)? {
                    LspHeader::ContentLength(len) => content_length = Some(len),
                    LspHeader::ContentType => (),
                };
            }
        };
    }

    let content_length = content_length
        .ok_or_else(|| anyhow!("missing content-length header: {}", buffer))?;

    let mut body_buffer = vec![0; content_length];
    reader.read_exact(&mut body_buffer)?;

    let body = String::from_utf8(body_buffer)?;
    Ok(body)
}

pub fn get_change_for_sync_kind(
    sync_kind: TextDocumentSyncKind,
    buffer: &Buffer,
    content_change: &TextDocumentContentChangeEvent,
) -> Option<Vec<TextDocumentContentChangeEvent>> {
    match sync_kind {
        TextDocumentSyncKind::NONE => None,
        TextDocumentSyncKind::FULL => {
            let text_document_content_change_event =
                TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text: buffer.get_document(),
                };
            Some(vec![text_document_content_change_event])
        }
        TextDocumentSyncKind::INCREMENTAL => Some(vec![content_change.clone()]),
        _ => None,
    }
}

/// What to tell the user when the server `command` exits on its own, given its last `stderr` lines.
fn stopped_message(command: &str, stderr: &str) -> String {
    let mut message = format!("`{command}` stopped unexpectedly.");
    if !stderr.is_empty() {
        message.push_str("\n\n");
        message.push_str(stderr);
    }
    // A rustup proxy whose component isn't installed, the usual case for rust-analyzer.
    if stderr.contains("Unknown binary") {
        let component = Path::new(command)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(command);
        message.push_str(&format!(
            "\n\nInstall it with `rustup component add {component}`, then restart Planck."
        ));
    }
    message
}

#[cfg(test)]
mod tests {
    use super::stopped_message;

    #[test]
    fn stopped_message_explains_a_missing_rustup_component() {
        let message = stopped_message(
            "rust-analyzer",
            "error: Unknown binary 'rust-analyzer' in official toolchain 'stable-x86_64-unknown-linux-gnu'.",
        );
        assert!(message.contains("Unknown binary 'rust-analyzer'"));
        assert!(message.contains("rustup component add rust-analyzer"));
        assert_eq!(
            stopped_message("gopls", ""),
            "`gopls` stopped unexpectedly."
        );
    }
}
