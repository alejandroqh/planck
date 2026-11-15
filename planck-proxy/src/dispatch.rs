use std::{
    collections::{HashMap, HashSet},
    fs,
    hash::{DefaultHasher, Hash, Hasher},
    io,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::Duration,
};

use alacritty_terminal::{event::WindowSize, event_loop::Msg};
use anyhow::{Context, Result, anyhow};
use crossbeam_channel::Sender;
use git2::{
    DiffOptions, ErrorCode::NotFound, Oid, Repository, build::CheckoutBuilder,
};
use grep_matcher::Matcher;
use grep_regex::RegexMatcherBuilder;
use grep_searcher::{SearcherBuilder, sinks::UTF8};
use indexmap::IndexMap;
use lapce_xi_rope::Rope;
use lsp_types::{
    CancelParams, MessageType, NumberOrString, Position, Range, ShowMessageParams,
    TextDocumentItem, Url,
    notification::{Cancel, Notification},
};
use parking_lot::Mutex;
use planck_rpc::{
    RequestId, RpcError,
    buffer::BufferId,
    core::{CoreNotification, CoreRpcHandler, FileChanged},
    file::FileNodeItem,
    file_line::FileLine,
    proxy::{
        ProxyHandler, ProxyNotification, ProxyRequest, ProxyResponse,
        ProxyRpcHandler, SearchMatch,
    },
    source_control::{BlameHunk, CommitInfo, DiffInfo, FileDiff, GraphCommit},
    style::{LineStyle, SemanticStyles},
    terminal::TermId,
};

use crate::{
    buffer::{Buffer, get_mod_time, load_file},
    plugin::{PluginCatalogRpcHandler, catalog::PluginCatalog},
    terminal::{Terminal, TerminalSender},
    watcher::{FileWatcher, Notify, WatchToken},
};

const OPEN_FILE_EVENT_TOKEN: WatchToken = WatchToken(1);
const WORKSPACE_EVENT_TOKEN: WatchToken = WatchToken(2);

pub struct Dispatcher {
    workspace: Option<PathBuf>,
    pub proxy_rpc: ProxyRpcHandler,
    core_rpc: CoreRpcHandler,
    catalog_rpc: PluginCatalogRpcHandler,
    buffers: HashMap<PathBuf, Buffer>,
    terminals: HashMap<TermId, TerminalSender>,
    file_watcher: FileWatcher,
    window_id: usize,
    tab_id: usize,
}

impl ProxyHandler for Dispatcher {
    fn handle_notification(&mut self, rpc: ProxyNotification) {
        use ProxyNotification::*;
        match rpc {
            Initialize {
                folders,
                lsp_servers,
                window_id,
                tab_id,
            } => {
                self.window_id = window_id;
                self.tab_id = tab_id;
                self.workspace = folders.first().cloned();
                self.file_watcher.notify(FileWatchNotifier::new(
                    self.workspace.clone(),
                    self.core_rpc.clone(),
                    self.proxy_rpc.clone(),
                ));
                for folder in &folders {
                    self.file_watcher.watch(folder, true, WORKSPACE_EVENT_TOKEN);
                }

                let plugin_rpc = self.catalog_rpc.clone();
                let workspace = self.workspace.clone();
                thread::spawn(move || {
                    let mut plugin = PluginCatalog::new(
                        workspace,
                        lsp_servers,
                        plugin_rpc.clone(),
                    );
                    plugin_rpc.mainloop(&mut plugin);
                });
                self.core_rpc.notification(CoreNotification::ProxyStatus {
                    status: planck_rpc::proxy::ProxyStatus::Connected,
                });

                // send home directory for initinal filepicker dir
                let dirs = directories::UserDirs::new();

                if let Some(dirs) = dirs {
                    self.core_rpc.home_dir(dirs.home_dir().into());
                }
            }
            OpenPaths { paths } => {
                self.core_rpc
                    .notification(CoreNotification::OpenPaths { paths });
            }
            OpenFileChanged { path } => {
                if path.exists() {
                    if let Some(buffer) = self.buffers.get(&path) {
                        if get_mod_time(&buffer.path) == buffer.mod_time {
                            return;
                        }
                        match load_file(&buffer.path) {
                            Ok(content) => {
                                self.core_rpc.open_file_changed(
                                    path,
                                    FileChanged::Change(content),
                                );
                            }
                            Err(err) => {
                                tracing::event!(
                                    tracing::Level::ERROR,
                                    "Failed to re-read file after change notification: {err}"
                                );
                            }
                        }
                    }
                } else {
                    self.buffers.remove(&path);
                    self.core_rpc.open_file_changed(path, FileChanged::Delete);
                }
            }
            Completion {
                request_id,
                path,
                input,
                position,
            } => {
                self.catalog_rpc
                    .completion(request_id, &path, input, position);
            }
            SignatureHelp {
                request_id,
                path,
                position,
            } => {
                self.catalog_rpc.signature_help(request_id, &path, position);
            }
            Shutdown {} => {
                self.catalog_rpc.shutdown();
                for (_, sender) in self.terminals.iter() {
                    sender.send(Msg::Shutdown);
                }
                self.proxy_rpc.shutdown();
            }
            Update { path, delta, rev } => {
                let buffer = self.buffers.get_mut(&path).unwrap();
                let old_text = buffer.rope.clone();
                buffer.update(&delta, rev);
                self.catalog_rpc.did_change_text_document(
                    &path,
                    rev,
                    delta,
                    old_text,
                    buffer.rope.clone(),
                );
            }
            UpdateLspServers { servers } => {
                if let Err(err) = self.catalog_rpc.update_lsp_servers(servers) {
                    tracing::error!("{:?}", err);
                }
            }
            NewTerminal { term_id, profile } => {
                let mut terminal = match Terminal::new(term_id, profile, 50, 10) {
                    Ok(terminal) => terminal,
                    Err(e) => {
                        self.core_rpc.terminal_launch_failed(term_id, e.to_string());
                        return;
                    }
                };

                #[allow(unused)]
                let mut child_id = None;

                #[cfg(target_os = "windows")]
                {
                    child_id = terminal.pty.child_watcher().pid().map(|x| x.get());
                }
                #[cfg(not(target_os = "windows"))]
                {
                    child_id = Some(terminal.pty.child().id());
                }

                self.core_rpc.terminal_process_id(term_id, child_id);
                let tx = terminal.tx.clone();
                let poller = terminal.poller.clone();
                let sender = TerminalSender::new(tx, poller);
                self.terminals.insert(term_id, sender);
                let rpc = self.core_rpc.clone();
                thread::spawn(move || {
                    terminal.run(rpc);
                });
            }
            TerminalWrite { term_id, content } => {
                if let Some(tx) = self.terminals.get(&term_id) {
                    tx.send(Msg::Input(content.into_bytes().into()));
                }
            }
            TerminalResize {
                term_id,
                width,
                height,
            } => {
                if let Some(tx) = self.terminals.get(&term_id) {
                    let size = WindowSize {
                        num_lines: height as u16,
                        num_cols: width as u16,
                        cell_width: 1,
                        cell_height: 1,
                    };

                    tx.send(Msg::Resize(size));
                }
            }
            TerminalClose { term_id } => {
                if let Some(tx) = self.terminals.remove(&term_id) {
                    tx.send(Msg::Shutdown);
                }
            }
            GitCommit { message, diffs } => {
                if let Some(workspace) = self.workspace.as_ref() {
                    match git_commit(workspace, &message, diffs) {
                        Ok(()) => (),
                        Err(e) => {
                            self.core_rpc.show_message(
                                "Git Commit failure".to_owned(),
                                ShowMessageParams {
                                    typ: MessageType::ERROR,
                                    message: e.to_string(),
                                },
                            );
                        }
                    }
                }
            }
            GitCheckout { reference } => {
                if let Some(workspace) = self.workspace.as_ref() {
                    match git_checkout(workspace, &reference) {
                        Ok(()) => (),
                        Err(e) => eprintln!("{e:?}"),
                    }
                }
            }
            GitDiscardFilesChanges { files } => {
                if let Some(workspace) = self.workspace.as_ref() {
                    match git_discard_files_changes(
                        workspace,
                        files.iter().map(AsRef::as_ref),
                    ) {
                        Ok(()) => (),
                        Err(e) => eprintln!("{e:?}"),
                    }
                }
            }
            GitDiscardWorkspaceChanges {} => {
                if let Some(workspace) = self.workspace.as_ref() {
                    match git_discard_workspace_changes(workspace) {
                        Ok(()) => (),
                        Err(e) => eprintln!("{e:?}"),
                    }
                }
            }
            GitInit {} => {
                if let Some(workspace) = self.workspace.as_ref() {
                    match git_init(workspace) {
                        Ok(()) => (),
                        Err(e) => eprintln!("{e:?}"),
                    }
                }
            }
            LspCancel { id } => {
                self.catalog_rpc.send_notification(
                    None,
                    Cancel::METHOD,
                    CancelParams {
                        id: NumberOrString::Number(id),
                    },
                    None,
                    None,
                    false,
                );
            }
        }
    }

    fn handle_request(&mut self, id: RequestId, rpc: ProxyRequest) {
        use ProxyRequest::*;
        match rpc {
            NewBuffer { buffer_id, path } => {
                let buffer = Buffer::new(buffer_id, path.clone());
                let content = buffer.rope.to_string();
                let read_only = buffer.read_only;
                self.catalog_rpc.did_open_document(
                    &path,
                    buffer.language_id.to_string(),
                    buffer.rev as i32,
                    content.clone(),
                );
                self.file_watcher.watch(&path, false, OPEN_FILE_EVENT_TOKEN);
                self.buffers.insert(path, buffer);
                self.respond_rpc(
                    id,
                    Ok(ProxyResponse::NewBufferResponse { content, read_only }),
                );
            }
            BufferHead { path, version } => {
                let result = file_at_version(&path, &version)
                    .map(|content| ProxyResponse::BufferHeadResponse {
                        version,
                        content,
                    })
                    .map_err(|e| RpcError {
                        code: 0,
                        message: format!("can't get file at version: {e}"),
                    });
                self.respond_rpc(id, result);
            }
            GlobalSearch {
                pattern,
                case_sensitive,
                whole_word,
                is_regex,
            } => {
                static WORKER_ID: AtomicU64 = AtomicU64::new(0);
                let our_id = WORKER_ID.fetch_add(1, Ordering::SeqCst) + 1;

                let workspace = self.workspace.clone();
                let buffers = self
                    .buffers
                    .iter()
                    .map(|p| p.0)
                    .cloned()
                    .collect::<Vec<PathBuf>>();
                let proxy_rpc = self.proxy_rpc.clone();

                // Perform the search on another thread to avoid blocking the proxy thread
                thread::spawn(move || {
                    proxy_rpc.handle_response(
                        id,
                        search_in_path(
                            our_id,
                            &WORKER_ID,
                            workspace
                                .iter()
                                .flat_map(|w| ignore::Walk::new(w).flatten())
                                .chain(
                                    buffers.iter().flat_map(|p| {
                                        ignore::Walk::new(p).flatten()
                                    }),
                                )
                                .map(|p| p.into_path())
                                // Open files inside the workspace come up twice; search them once.
                                .filter({
                                    let mut seen = HashSet::new();
                                    move |p| seen.insert(p.clone())
                                }),
                            &pattern,
                            case_sensitive,
                            whole_word,
                            is_regex,
                        ),
                    );
                });
            }
            CompletionResolve {
                plugin_id,
                completion_item,
            } => {
                let proxy_rpc = self.proxy_rpc.clone();
                self.catalog_rpc.completion_resolve(
                    plugin_id,
                    *completion_item,
                    move |result| {
                        let result = result.map(|item| {
                            ProxyResponse::CompletionResolveResponse {
                                item: Box::new(item),
                            }
                        });
                        proxy_rpc.handle_response(id, result);
                    },
                );
            }
            GetHover {
                request_id,
                path,
                position,
            } => {
                let proxy_rpc = self.proxy_rpc.clone();
                self.catalog_rpc.hover(&path, position, move |_, result| {
                    let result = result.map(|hover| ProxyResponse::HoverResponse {
                        request_id,
                        hover,
                    });
                    proxy_rpc.handle_response(id, result);
                });
            }
            GetSignature { .. } => {}
            GetReferences { path, position } => {
                let proxy_rpc = self.proxy_rpc.clone();
                self.catalog_rpc.get_references(
                    &path,
                    position,
                    move |_, result| {
                        let result = result.map(|references| {
                            ProxyResponse::GetReferencesResponse { references }
                        });
                        proxy_rpc.handle_response(id, result);
                    },
                );
            }
            GitBlame { path } => {
                let Some(buffer) = self.buffers.get(&path) else {
                    self.respond_rpc(
                        id,
                        Err(RpcError {
                            code: 0,
                            message: "buffer not open".to_string(),
                        }),
                    );
                    return;
                };
                let content = buffer.rope.clone();
                let proxy_rpc = self.proxy_rpc.clone();
                // Blaming a long history can take a while, so keep the proxy thread free.
                thread::spawn(move || {
                    let result = git_blame(&path, &content.to_string())
                        .map(|hunks| ProxyResponse::GitBlameResponse { hunks })
                        .map_err(|e| RpcError {
                            code: 0,
                            message: e.to_string(),
                        });
                    proxy_rpc.handle_response(id, result);
                });
            }
            GitFileCommits { path } => {
                let proxy_rpc = self.proxy_rpc.clone();
                thread::spawn(move || {
                    let result = git_file_commits(&path, 200)
                        .map(|commits| ProxyResponse::GitFileCommitsResponse {
                            commits,
                        })
                        .map_err(|e| RpcError {
                            code: 0,
                            message: e.to_string(),
                        });
                    proxy_rpc.handle_response(id, result);
                });
            }
            GitGraph {} => {
                let Some(workspace) = self.workspace.clone() else {
                    self.respond_rpc(
                        id,
                        Err(RpcError {
                            code: 0,
                            message: "no workspace".to_string(),
                        }),
                    );
                    return;
                };
                let proxy_rpc = self.proxy_rpc.clone();
                thread::spawn(move || {
                    // shortcut: the newest 1000 commits only; load more on scroll if older history is wanted.
                    let result = git_graph(&workspace, 1000)
                        .map(|commits| ProxyResponse::GitGraphResponse { commits })
                        .map_err(|e| RpcError {
                            code: 0,
                            message: e.to_string(),
                        });
                    proxy_rpc.handle_response(id, result);
                });
            }
            GitGetRemoteFileUrl { file } => {
                if let Some(workspace) = self.workspace.as_ref() {
                    match git_get_remote_file_url(workspace, &file) {
                        Ok(s) => self.proxy_rpc.handle_response(
                            id,
                            Ok(ProxyResponse::GitGetRemoteFileUrl { file_url: s }),
                        ),
                        Err(e) => eprintln!("{e:?}"),
                    }
                }
            }
            GetDefinition {
                request_id,
                path,
                position,
            } => {
                let proxy_rpc = self.proxy_rpc.clone();
                self.catalog_rpc.get_definition(
                    &path,
                    position,
                    move |_, result| {
                        let result = result.map(|definition| {
                            ProxyResponse::GetDefinitionResponse {
                                request_id,
                                definition,
                            }
                        });
                        proxy_rpc.handle_response(id, result);
                    },
                );
            }
            GetTypeDefinition {
                request_id,
                path,
                position,
            } => {
                let proxy_rpc = self.proxy_rpc.clone();
                self.catalog_rpc.get_type_definition(
                    &path,
                    position,
                    move |_, result| {
                        let result = result.map(|definition| {
                            ProxyResponse::GetTypeDefinition {
                                request_id,
                                definition,
                            }
                        });
                        proxy_rpc.handle_response(id, result);
                    },
                );
            }
            ShowCallHierarchy { path, position } => {
                let proxy_rpc = self.proxy_rpc.clone();
                self.catalog_rpc.show_call_hierarchy(
                    &path,
                    position,
                    move |_, result| {
                        let result = result.map(|items| {
                            ProxyResponse::ShowCallHierarchyResponse { items }
                        });
                        proxy_rpc.handle_response(id, result);
                    },
                );
            }
            CallHierarchyIncoming {
                path,
                call_hierarchy_item,
            } => {
                let proxy_rpc = self.proxy_rpc.clone();
                self.catalog_rpc.call_hierarchy_incoming(
                    &path,
                    call_hierarchy_item,
                    move |_, result| {
                        let result = result.map(|items| {
                            ProxyResponse::CallHierarchyIncomingResponse { items }
                        });
                        proxy_rpc.handle_response(id, result);
                    },
                );
            }
            GetInlayHints { path } => {
                let proxy_rpc = self.proxy_rpc.clone();
                let buffer = self.buffers.get(&path).unwrap();
                let range = Range {
                    start: Position::new(0, 0),
                    end: buffer.offset_to_position(buffer.len()),
                };
                self.catalog_rpc
                    .get_inlay_hints(&path, range, move |_, result| {
                        let result = result
                            .map(|hints| ProxyResponse::GetInlayHints { hints });
                        proxy_rpc.handle_response(id, result);
                    });
            }
            GetInlineCompletions {
                path,
                position,
                trigger_kind,
            } => {
                let proxy_rpc = self.proxy_rpc.clone();
                self.catalog_rpc.get_inline_completions(
                    &path,
                    position,
                    trigger_kind,
                    move |_, result| {
                        let result = result.map(|completions| {
                            ProxyResponse::GetInlineCompletions { completions }
                        });
                        proxy_rpc.handle_response(id, result);
                    },
                );
            }
            GetSemanticTokens { path } => {
                let buffer = self.buffers.get(&path).unwrap();
                let text = buffer.rope.clone();
                let rev = buffer.rev;
                let len = buffer.len();
                let local_path = path.clone();
                let proxy_rpc = self.proxy_rpc.clone();
                let catalog_rpc = self.catalog_rpc.clone();

                let handle_tokens =
                    move |result: Result<Vec<LineStyle>, RpcError>| match result {
                        Ok(styles) => {
                            proxy_rpc.handle_response(
                                id,
                                Ok(ProxyResponse::GetSemanticTokens {
                                    styles: SemanticStyles {
                                        rev,
                                        path: local_path,
                                        styles,
                                        len,
                                    },
                                }),
                            );
                        }
                        Err(e) => {
                            proxy_rpc.handle_response(id, Err(e));
                        }
                    };

                let proxy_rpc = self.proxy_rpc.clone();
                self.catalog_rpc.get_semantic_tokens(
                    &path,
                    move |plugin_id, result| match result {
                        Ok(result) => {
                            catalog_rpc.format_semantic_tokens(
                                plugin_id,
                                result,
                                text,
                                Box::new(handle_tokens),
                            );
                        }
                        Err(e) => {
                            proxy_rpc.handle_response(id, Err(e));
                        }
                    },
                );
            }
            GetCodeActions {
                path,
                position,
                diagnostics,
            } => {
                let proxy_rpc = self.proxy_rpc.clone();
                self.catalog_rpc.get_code_actions(
                    &path,
                    position,
                    diagnostics,
                    move |plugin_id, result| {
                        let result = result.map(|resp| {
                            ProxyResponse::GetCodeActionsResponse { plugin_id, resp }
                        });
                        proxy_rpc.handle_response(id, result);
                    },
                );
            }
            GetDocumentSymbols { path } => {
                let proxy_rpc = self.proxy_rpc.clone();
                self.catalog_rpc
                    .get_document_symbols(&path, move |_, result| {
                        let result = result
                            .map(|resp| ProxyResponse::GetDocumentSymbols { resp });
                        proxy_rpc.handle_response(id, result);
                    });
            }
            GetWorkspaceSymbols { query } => {
                let proxy_rpc = self.proxy_rpc.clone();
                self.catalog_rpc
                    .get_workspace_symbols(query, move |_, result| {
                        let result = result.map(|symbols| {
                            ProxyResponse::GetWorkspaceSymbols { symbols }
                        });
                        proxy_rpc.handle_response(id, result);
                    });
            }
            GetDocumentFormatting { path } => {
                let proxy_rpc = self.proxy_rpc.clone();
                self.catalog_rpc
                    .get_document_formatting(&path, move |_, result| {
                        let result = result.map(|edits| {
                            ProxyResponse::GetDocumentFormatting { edits }
                        });
                        proxy_rpc.handle_response(id, result);
                    });
            }
            PrepareRename { path, position } => {
                let proxy_rpc = self.proxy_rpc.clone();
                self.catalog_rpc.prepare_rename(
                    &path,
                    position,
                    move |_, result| {
                        let result =
                            result.map(|resp| ProxyResponse::PrepareRename { resp });
                        proxy_rpc.handle_response(id, result);
                    },
                );
            }
            Rename {
                path,
                position,
                new_name,
            } => {
                let proxy_rpc = self.proxy_rpc.clone();
                self.catalog_rpc.rename(
                    &path,
                    position,
                    new_name,
                    move |_, result| {
                        let result =
                            result.map(|edit| ProxyResponse::Rename { edit });
                        proxy_rpc.handle_response(id, result);
                    },
                );
            }
            GetFiles { .. } => {
                let workspace = self.workspace.clone();
                let proxy_rpc = self.proxy_rpc.clone();
                thread::spawn(move || {
                    let result = if let Some(workspace) = workspace {
                        let git_folder =
                            ignore::overrides::OverrideBuilder::new(&workspace)
                                .add("!.git/")
                                .map(|git_folder| git_folder.build());

                        let walker = match git_folder {
                            Ok(Ok(git_folder)) => {
                                ignore::WalkBuilder::new(&workspace)
                                    .hidden(false)
                                    .parents(false)
                                    .require_git(false)
                                    .overrides(git_folder)
                                    .build()
                            }
                            _ => ignore::WalkBuilder::new(&workspace)
                                .parents(false)
                                .require_git(false)
                                .build(),
                        };

                        let mut items = Vec::new();
                        for path in walker.flatten() {
                            if let Some(file_type) = path.file_type() {
                                if file_type.is_file() {
                                    items.push(path.into_path());
                                }
                            }
                        }
                        Ok(ProxyResponse::GetFilesResponse { items })
                    } else {
                        Ok(ProxyResponse::GetFilesResponse { items: Vec::new() })
                    };
                    proxy_rpc.handle_response(id, result);
                });
            }
            GetOpenFilesContent {} => {
                let items = self
                    .buffers
                    .iter()
                    .map(|(path, buffer)| TextDocumentItem {
                        uri: Url::from_file_path(path).unwrap(),
                        language_id: buffer.language_id.to_string(),
                        version: buffer.rev as i32,
                        text: buffer.get_document(),
                    })
                    .collect();
                let resp = ProxyResponse::GetOpenFilesContentResponse { items };
                self.proxy_rpc.handle_response(id, Ok(resp));
            }
            ReadDir { path } => {
                let proxy_rpc = self.proxy_rpc.clone();
                thread::spawn(move || {
                    let path = match path {
                        planck_rpc::proxy::PathRequest::Home => {
                            std::env::home_dir().unwrap_or(PathBuf::from("/"))
                        }
                        planck_rpc::proxy::PathRequest::Path(path_buf) => path_buf,
                    };
                    let result = fs::read_dir(&path)
                        .map(|entries| {
                            let mut items = entries
                                .into_iter()
                                .filter_map(|entry| {
                                    entry
                                        .map(|e| FileNodeItem {
                                            path: e.path(),
                                            is_dir: e.path().is_dir(),
                                            open: false,
                                            read: false,
                                            children: HashMap::new(),
                                            children_open_count: 0,
                                        })
                                        .ok()
                                })
                                .collect::<Vec<FileNodeItem>>();

                            items.sort();

                            ProxyResponse::ReadDirResponse {
                                items,
                                dir: FileNodeItem {
                                    path: path.clone(),
                                    is_dir: path.is_dir(),
                                    read: false,
                                    open: false,
                                    children: HashMap::new(),
                                    children_open_count: 0,
                                },
                            }
                        })
                        .map_err(|e| RpcError {
                            code: 0,
                            message: format!(
                                "Error reading directory {:?}: {}",
                                &path,
                                e.to_string()
                            ),
                        });
                    proxy_rpc.handle_response(id, result);
                });
            }
            Save {
                rev,
                path,
                create_parents,
            } => {
                let buffer = self.buffers.get_mut(&path).unwrap();
                let result = buffer
                    .save(rev, create_parents)
                    .map(|_r| {
                        self.catalog_rpc
                            .did_save_text_document(&path, buffer.rope.clone());
                        ProxyResponse::SaveResponse {}
                    })
                    .map_err(|e| RpcError {
                        code: 0,
                        message: e.to_string(),
                    });
                self.respond_rpc(id, result);
            }
            SaveBufferAs {
                buffer_id,
                path,
                rev,
                content,
                create_parents,
            } => {
                let mut buffer = Buffer::new(buffer_id, path.clone());
                buffer.rope = Rope::from(content);
                buffer.rev = rev;
                let result = buffer
                    .save(rev, create_parents)
                    .map(|_| ProxyResponse::Success {})
                    .map_err(|e| RpcError {
                        code: 0,
                        message: e.to_string(),
                    });
                self.buffers.insert(path, buffer);
                self.respond_rpc(id, result);
            }
            CreateFile { path } => {
                let result = path
                    .parent()
                    .map_or(Ok(()), std::fs::create_dir_all)
                    .and_then(|()| {
                        std::fs::OpenOptions::new()
                            .write(true)
                            .create_new(true)
                            .open(path)
                    })
                    .map(|_| ProxyResponse::Success {})
                    .map_err(|e| RpcError {
                        code: 0,
                        message: e.to_string(),
                    });
                self.respond_rpc(id, result);
            }
            CreateDirectory { path } => {
                let result = std::fs::create_dir_all(path)
                    .map(|_| ProxyResponse::Success {})
                    .map_err(|e| RpcError {
                        code: 0,
                        message: e.to_string(),
                    });
                self.respond_rpc(id, result);
            }
            TrashPath { path } => {
                let result = trash::delete(path)
                    .map(|_| ProxyResponse::Success {})
                    .map_err(|e| RpcError {
                        code: 0,
                        message: e.to_string(),
                    });
                self.respond_rpc(id, result);
            }
            DuplicatePath {
                existing_path,
                new_path,
            } => {
                // We first check if the destination already exists, because copy can overwrite it
                // and that's not the default behavior we want for when a user duplicates a document.
                let result = if new_path.exists() {
                    Err(RpcError {
                        code: 0,
                        message: format!("{new_path:?} already exists"),
                    })
                } else {
                    if let Some(parent) = new_path.parent() {
                        if let Err(error) = std::fs::create_dir_all(parent) {
                            let result = Err(RpcError {
                                code: 0,
                                message: error.to_string(),
                            });
                            self.respond_rpc(id, result);
                            return;
                        }
                    }
                    std::fs::copy(existing_path, new_path)
                        .map(|_| ProxyResponse::Success {})
                        .map_err(|e| RpcError {
                            code: 0,
                            message: e.to_string(),
                        })
                };
                self.respond_rpc(id, result);
            }
            RenamePath { from, to } => {
                // We first check if the destination already exists, because rename can overwrite it
                // and that's not the default behavior we want for when a user renames a document.
                let result = if to.exists() {
                    Err(format!("{} already exists", to.display()))
                } else {
                    Ok(())
                };

                let result = result.and_then(|_| {
                    if let Some(parent) = to.parent() {
                        fs::create_dir_all(parent).map_err(|e| {
                            if let io::ErrorKind::AlreadyExists = e.kind() {
                                format!(
                                    "{} has a parent that is not a directory",
                                    to.display()
                                )
                            } else {
                                e.to_string()
                            }
                        })
                    } else {
                        Ok(())
                    }
                });

                let result = result
                    .and_then(|_| fs::rename(&from, &to).map_err(|e| e.to_string()));

                let result = result
                    .map(|_| {
                        let to = to.canonicalize().unwrap_or(to);

                        let (is_dir, is_file) = to
                            .metadata()
                            .map(|metadata| (metadata.is_dir(), metadata.is_file()))
                            .unwrap_or((false, false));

                        if is_dir {
                            // Update all buffers in which a file the renamed directory is an
                            // ancestor of is open to use the file's new path.
                            // This could be written more nicely if `HashMap::extract_if` were
                            // stable.
                            let child_buffers: Vec<_> = self
                                .buffers
                                .keys()
                                .filter_map(|path| {
                                    path.strip_prefix(&from).ok().map(|suffix| {
                                        (path.clone(), suffix.to_owned())
                                    })
                                })
                                .collect();

                            for (path, suffix) in child_buffers {
                                if let Some(mut buffer) = self.buffers.remove(&path)
                                {
                                    let new_path = to.join(suffix);
                                    buffer.path = new_path;

                                    self.buffers.insert(buffer.path.clone(), buffer);
                                }
                            }
                        } else if is_file {
                            // If the renamed file is open in a buffer, update it to use the new
                            // path.
                            let buffer = self.buffers.remove(&from);

                            if let Some(mut buffer) = buffer {
                                buffer.path.clone_from(&to);
                                self.buffers.insert(to.clone(), buffer);
                            }
                        }

                        ProxyResponse::CreatePathResponse { path: to }
                    })
                    .map_err(|message| RpcError { code: 0, message });

                self.respond_rpc(id, result);
            }
            TestCreateAtPath { path } => {
                // This performs a best effort test to see if an attempt to create an item at
                // `path` or rename an item to `path` will succeed.
                // Currently the only conditions that are tested are that `path` doesn't already
                // exist and that `path` doesn't have a parent that exists and is not a directory.
                let result = if path.exists() {
                    Err(format!("{} already exists", path.display()))
                } else {
                    Ok(path)
                };

                let result = result
                    .and_then(|path| {
                        let parent_is_dir = path
                            .ancestors()
                            .skip(1)
                            .find(|parent| parent.exists())
                            .is_none_or(|parent| parent.is_dir());

                        if parent_is_dir {
                            Ok(ProxyResponse::Success {})
                        } else {
                            Err(format!(
                                "{} has a parent that is not a directory",
                                path.display()
                            ))
                        }
                    })
                    .map_err(|message| RpcError { code: 0, message });

                self.respond_rpc(id, result);
            }
            GetSelectionRange { positions, path } => {
                let proxy_rpc = self.proxy_rpc.clone();
                self.catalog_rpc.get_selection_range(
                    path.as_path(),
                    positions,
                    move |_, result| {
                        let result = result.map(|ranges| {
                            ProxyResponse::GetSelectionRange { ranges }
                        });
                        proxy_rpc.handle_response(id, result);
                    },
                );
            }
            CodeActionResolve {
                action_item,
                plugin_id,
            } => {
                let proxy_rpc = self.proxy_rpc.clone();
                self.catalog_rpc.action_resolve(
                    *action_item,
                    plugin_id,
                    move |result| {
                        let result = result.map(|item| {
                            ProxyResponse::CodeActionResolveResponse {
                                item: Box::new(item),
                            }
                        });
                        proxy_rpc.handle_response(id, result);
                    },
                );
            }
            GetCodeLens { path } => {
                let proxy_rpc = self.proxy_rpc.clone();
                self.catalog_rpc
                    .get_code_lens(&path, move |plugin_id, result| {
                        let result = result.map(|resp| {
                            ProxyResponse::GetCodeLensResponse { plugin_id, resp }
                        });
                        proxy_rpc.handle_response(id, result);
                    });
            }
            LspFoldingRange { path } => {
                let proxy_rpc = self.proxy_rpc.clone();
                self.catalog_rpc.get_lsp_folding_range(
                    &path,
                    move |plugin_id, result| {
                        let result = result.map(|resp| {
                            ProxyResponse::LspFoldingRangeResponse {
                                plugin_id,
                                resp,
                            }
                        });
                        proxy_rpc.handle_response(id, result);
                    },
                );
            }
            GetCodeLensResolve { code_lens, path } => {
                let proxy_rpc = self.proxy_rpc.clone();
                self.catalog_rpc.get_code_lens_resolve(
                    &path,
                    &code_lens,
                    move |plugin_id, result| {
                        let result = result.map(|resp| {
                            ProxyResponse::GetCodeLensResolveResponse {
                                plugin_id,
                                resp,
                            }
                        });
                        proxy_rpc.handle_response(id, result);
                    },
                );
            }
            GotoImplementation { path, position } => {
                let proxy_rpc = self.proxy_rpc.clone();
                self.catalog_rpc.go_to_implementation(
                    &path,
                    position,
                    move |plugin_id, result| {
                        let result = result.map(|resp| {
                            ProxyResponse::GotoImplementationResponse {
                                plugin_id,
                                resp,
                            }
                        });
                        proxy_rpc.handle_response(id, result);
                    },
                );
            }
            ReferencesResolve { items } => {
                let items: Vec<FileLine> = items
                    .into_iter()
                    .filter_map(|location| {
                        let Ok(path) = location.uri.to_file_path() else {
                            tracing::error!(
                                "get file path fail: {:?}",
                                location.uri
                            );
                            return None;
                        };
                        let buffer = self.get_buffer_or_insert(path.clone());
                        let line_num = location.range.start.line as usize;
                        let content = buffer.line_to_cow(line_num).to_string();
                        Some(FileLine {
                            path,
                            position: location.range.start,
                            content,
                        })
                    })
                    .collect();
                let resp = ProxyResponse::ReferencesResolveResponse { items };
                self.proxy_rpc.handle_response(id, Ok(resp));
            }
            GetDocumentHighlights { path, position } => {
                let proxy_rpc = self.proxy_rpc.clone();
                self.catalog_rpc.get_document_highlights(
                    &path,
                    position,
                    move |_, result| {
                        let result = result.map(|highlights| {
                            ProxyResponse::GetDocumentHighlights { highlights }
                        });
                        proxy_rpc.handle_response(id, result);
                    },
                );
            }
        }
    }
}

impl Dispatcher {
    pub fn new(core_rpc: CoreRpcHandler, proxy_rpc: ProxyRpcHandler) -> Self {
        let plugin_rpc =
            PluginCatalogRpcHandler::new(core_rpc.clone(), proxy_rpc.clone());

        let file_watcher = FileWatcher::new();

        Self {
            workspace: None,
            proxy_rpc,
            core_rpc,
            catalog_rpc: plugin_rpc,
            buffers: HashMap::new(),
            terminals: HashMap::new(),
            file_watcher,
            window_id: 1,
            tab_id: 1,
        }
    }

    fn respond_rpc(&self, id: RequestId, result: Result<ProxyResponse, RpcError>) {
        self.proxy_rpc.handle_response(id, result);
    }

    fn get_buffer_or_insert(&mut self, path: PathBuf) -> &mut Buffer {
        self.buffers
            .entry(path.clone())
            .or_insert(Buffer::new(BufferId::next(), path))
    }
}

struct FileWatchNotifier {
    core_rpc: CoreRpcHandler,
    proxy_rpc: ProxyRpcHandler,
    workspace: Option<PathBuf>,
    workspace_fs_change_handler: Arc<Mutex<Option<Sender<bool>>>>,
    last_diff: Arc<Mutex<DiffInfo>>,
}

impl Notify for FileWatchNotifier {
    fn notify(&self, events: Vec<(WatchToken, notify::Event)>) {
        self.handle_fs_events(events);
    }
}

impl FileWatchNotifier {
    fn new(
        workspace: Option<PathBuf>,
        core_rpc: CoreRpcHandler,
        proxy_rpc: ProxyRpcHandler,
    ) -> Self {
        let notifier = Self {
            workspace,
            core_rpc,
            proxy_rpc,
            workspace_fs_change_handler: Arc::new(Mutex::new(None)),
            last_diff: Arc::new(Mutex::new(DiffInfo::default())),
        };

        if let Some(workspace) = notifier.workspace.clone() {
            let core_rpc = notifier.core_rpc.clone();
            let last_diff = notifier.last_diff.clone();
            thread::spawn(move || {
                if let Some(diff) = git_diff_new(&workspace) {
                    core_rpc.diff_info(diff.clone());
                    *last_diff.lock() = diff;
                }
            });
        }

        notifier
    }

    fn handle_fs_events(&self, events: Vec<(WatchToken, notify::Event)>) {
        for (token, event) in events {
            match token {
                OPEN_FILE_EVENT_TOKEN => self.handle_open_file_fs_event(event),
                WORKSPACE_EVENT_TOKEN => self.handle_workspace_fs_event(event),
                _ => {}
            }
        }
    }

    fn handle_open_file_fs_event(&self, event: notify::Event) {
        if event.kind.is_modify() || event.kind.is_remove() {
            for path in event.paths {
                #[cfg(windows)]
                if let Some(path_str) = path.to_str() {
                    const PREFIX: &str = r"\\?\";
                    if let Some(path_str) = path_str.strip_prefix(PREFIX) {
                        let path = PathBuf::from(&path_str);
                        self.proxy_rpc.notification(
                            ProxyNotification::OpenFileChanged { path },
                        );
                        continue;
                    }
                }
                self.proxy_rpc
                    .notification(ProxyNotification::OpenFileChanged { path });
            }
        }
    }

    fn handle_workspace_fs_event(&self, event: notify::Event) {
        let explorer_change = match &event.kind {
            notify::EventKind::Create(_)
            | notify::EventKind::Remove(_)
            | notify::EventKind::Modify(notify::event::ModifyKind::Name(_)) => true,
            notify::EventKind::Modify(_) => false,
            _ => return,
        };

        let mut handler = self.workspace_fs_change_handler.lock();
        if let Some(sender) = handler.as_mut() {
            if explorer_change {
                // only send the value if we need to update file explorer as well
                if let Err(err) = sender.send(explorer_change) {
                    tracing::error!("{:?}", err);
                }
            }
            return;
        }
        let (sender, receiver) = crossbeam_channel::unbounded();
        if explorer_change {
            // only send the value if we need to update file explorer as well
            if let Err(err) = sender.send(explorer_change) {
                tracing::error!("{:?}", err);
            }
        }

        let local_handler = self.workspace_fs_change_handler.clone();
        let core_rpc = self.core_rpc.clone();
        let workspace = self.workspace.clone().unwrap();
        let last_diff = self.last_diff.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(500));

            {
                local_handler.lock().take();
            }

            let mut explorer_change = false;
            for e in receiver {
                if e {
                    explorer_change = true;
                    break;
                }
            }
            if explorer_change {
                core_rpc.workspace_file_change();
            }
            if let Some(diff) = git_diff_new(&workspace) {
                let mut last_diff = last_diff.lock();
                if diff != *last_diff {
                    core_rpc.diff_info(diff.clone());
                    *last_diff = diff;
                }
            }
        });
        *handler = Some(sender);
    }
}

#[derive(Clone, Debug)]
pub struct DiffHunk {
    pub old_start: u32,
    pub old_lines: u32,
    pub new_start: u32,
    pub new_lines: u32,
    pub header: String,
}

fn git_init(workspace_path: &Path) -> Result<()> {
    if Repository::discover(workspace_path).is_err() {
        Repository::init(workspace_path)?;
    };
    Ok(())
}

fn git_commit(
    workspace_path: &Path,
    message: &str,
    diffs: Vec<FileDiff>,
) -> Result<()> {
    let repo = Repository::discover(workspace_path)?;
    let mut index = repo.index()?;
    for diff in diffs {
        match diff {
            FileDiff::Modified(p) | FileDiff::Added(p) => {
                index.add_path(p.strip_prefix(workspace_path)?)?;
            }
            FileDiff::Renamed(a, d) => {
                index.add_path(a.strip_prefix(workspace_path)?)?;
                index.remove_path(d.strip_prefix(workspace_path)?)?;
            }
            FileDiff::Deleted(p) => {
                index.remove_path(p.strip_prefix(workspace_path)?)?;
            }
        }
    }
    index.write()?;
    let tree = index.write_tree()?;
    let tree = repo.find_tree(tree)?;

    match repo.signature() {
        Ok(signature) => {
            let parents = repo
                .head()
                .and_then(|head| Ok(vec![head.peel_to_commit()?]))
                .unwrap_or(vec![]);
            let parents_refs = parents.iter().collect::<Vec<_>>();

            repo.commit(
                Some("HEAD"),
                &signature,
                &signature,
                message,
                &tree,
                &parents_refs,
            )?;
            Ok(())
        }
        Err(e) => match e.code() {
            NotFound => Err(anyhow!(
                "No user.name and/or user.email configured for this git repository."
            )),
            _ => Err(anyhow!(
                "Error while creating commit's signature: {}",
                e.message()
            )),
        },
    }
}

fn git_checkout(workspace_path: &Path, reference: &str) -> Result<()> {
    let repo = Repository::discover(workspace_path)?;
    let (object, reference) = repo.revparse_ext(reference)?;
    repo.checkout_tree(&object, None)?;
    repo.set_head(reference.unwrap().name().unwrap())?;
    Ok(())
}

fn git_discard_files_changes<'a>(
    workspace_path: &Path,
    files: impl Iterator<Item = &'a Path>,
) -> Result<()> {
    let repo = Repository::discover(workspace_path)?;

    let mut checkout_b = CheckoutBuilder::new();
    checkout_b.update_only(false).force();

    let mut had_path = false;
    for path in files {
        // Remove the workspace path so it is relative to the folder
        if let Ok(path) = path.strip_prefix(workspace_path) {
            had_path = true;
            checkout_b.path(path);
        }
    }

    if !had_path {
        // If there we no paths then we do nothing
        // because the default behavior of checkout builder is to select all files
        // if it is not given a path
        return Ok(());
    }

    repo.checkout_index(None, Some(&mut checkout_b))?;

    Ok(())
}

fn git_discard_workspace_changes(workspace_path: &Path) -> Result<()> {
    let repo = Repository::discover(workspace_path)?;
    let mut checkout_b = CheckoutBuilder::new();
    checkout_b.force();

    repo.checkout_index(None, Some(&mut checkout_b))?;

    Ok(())
}

fn git_delta_format(
    workspace_path: &Path,
    delta: &git2::DiffDelta,
) -> Option<(git2::Delta, git2::Oid, PathBuf)> {
    match delta.status() {
        git2::Delta::Added | git2::Delta::Untracked => Some((
            git2::Delta::Added,
            delta.new_file().id(),
            delta.new_file().path().map(|p| workspace_path.join(p))?,
        )),
        git2::Delta::Deleted => Some((
            git2::Delta::Deleted,
            delta.old_file().id(),
            delta.old_file().path().map(|p| workspace_path.join(p))?,
        )),
        git2::Delta::Modified => Some((
            git2::Delta::Modified,
            delta.new_file().id(),
            delta.new_file().path().map(|p| workspace_path.join(p))?,
        )),
        _ => None,
    }
}

fn git_diff_new(workspace_path: &Path) -> Option<DiffInfo> {
    let repo = Repository::discover(workspace_path).ok()?;
    let name = match repo.head() {
        Ok(head) => head.shorthand().ok()?.to_string(),
        _ => "(No branch)".to_owned(),
    };

    let mut branches = Vec::new();
    for branch in repo.branches(None).ok()? {
        branches.push(branch.ok()?.0.name().ok()??.to_string());
    }

    let mut tags = Vec::new();
    if let Ok(git_tags) = repo.tag_names(None) {
        for tag in git_tags.into_iter().flatten() {
            let Some(tag) = tag else {
                continue;
            };
            tags.push(tag.to_owned());
        }
    }

    let mut deltas = Vec::new();
    let mut diff_options = DiffOptions::new();
    let diff = repo
        .diff_index_to_workdir(
            None,
            Some(
                diff_options
                    .include_untracked(true)
                    .recurse_untracked_dirs(true),
            ),
        )
        .ok()?;
    for delta in diff.deltas() {
        if let Some(delta) = git_delta_format(workspace_path, &delta) {
            deltas.push(delta);
        }
    }

    let oid = match repo.revparse_single("HEAD^{tree}") {
        Ok(obj) => obj.id(),
        _ => Oid::ZERO_SHA1,
    };

    let cached_diff = repo
        .diff_tree_to_index(repo.find_tree(oid).ok().as_ref(), None, None)
        .ok();

    if let Some(cached_diff) = cached_diff {
        for delta in cached_diff.deltas() {
            if let Some(delta) = git_delta_format(workspace_path, &delta) {
                deltas.push(delta);
            }
        }
    }
    let mut renames = Vec::new();
    let mut renamed_deltas = HashSet::new();

    for (added_index, delta) in deltas.iter().enumerate() {
        if delta.0 == git2::Delta::Added {
            for (deleted_index, d) in deltas.iter().enumerate() {
                if d.0 == git2::Delta::Deleted && d.1 == delta.1 {
                    renames.push((added_index, deleted_index));
                    renamed_deltas.insert(added_index);
                    renamed_deltas.insert(deleted_index);
                    break;
                }
            }
        }
    }

    let mut file_diffs = Vec::new();
    for (added_index, deleted_index) in renames.iter() {
        file_diffs.push(FileDiff::Renamed(
            deltas[*added_index].2.clone(),
            deltas[*deleted_index].2.clone(),
        ));
    }
    for (i, delta) in deltas.iter().enumerate() {
        if renamed_deltas.contains(&i) {
            continue;
        }
        let diff = match delta.0 {
            git2::Delta::Added => FileDiff::Added(delta.2.clone()),
            git2::Delta::Deleted => FileDiff::Deleted(delta.2.clone()),
            git2::Delta::Modified => FileDiff::Modified(delta.2.clone()),
            _ => continue,
        };
        file_diffs.push(diff);
    }
    file_diffs.sort_by_key(|d| match d {
        FileDiff::Modified(p)
        | FileDiff::Added(p)
        | FileDiff::Renamed(p, _)
        | FileDiff::Deleted(p) => p.clone(),
    });
    let mut refs = DefaultHasher::new();
    for reference in repo.references().ok()?.flatten() {
        (reference.name_bytes(), reference.target()).hash(&mut refs);
    }
    repo.head().ok().and_then(|h| h.target()).hash(&mut refs);
    Some(DiffInfo {
        head: name,
        branches,
        tags,
        diffs: file_diffs,
        refs_hash: refs.finish(),
    })
}

/// The content of `path` at `version`: "head" or a git revision such as a branch or commit.
fn file_at_version(path: &Path, version: &str) -> Result<String> {
    let repo = Repository::discover(path.parent().unwrap_or(path))?;
    let workdir = repo.workdir().ok_or_else(|| anyhow!("bare repository"))?;
    let spec = if version == "head" { "HEAD" } else { version };
    let tree = repo.revparse_single(spec)?.peel_to_tree()?;
    let blob = repo.find_blob(tree.get_path(path.strip_prefix(workdir)?)?.id())?;
    Ok(std::str::from_utf8(blob.content())
        .context("content bytes to string")?
        .to_string())
}

/// The newest commits reachable from HEAD that changed `path`, at most `limit`.
fn git_file_commits(path: &Path, limit: usize) -> Result<Vec<CommitInfo>> {
    let repo = Repository::discover(path.parent().unwrap_or(path))?;
    let workdir = repo.workdir().ok_or_else(|| anyhow!("bare repository"))?;
    let path = path.strip_prefix(workdir)?;
    let you = repo.config()?.get_string("user.email").ok();
    let file_id = |commit: &git2::Commit| {
        commit
            .tree()
            .ok()?
            .get_path(path)
            .ok()
            .map(|entry| entry.id())
    };
    let mut walk = repo.revwalk()?;
    walk.push_head()?;
    walk.set_sorting(git2::Sort::TIME)?;
    let mut commits = Vec::new();
    for id in walk {
        let commit = repo.find_commit(id?)?;
        let id = file_id(&commit);
        // Changed: differs from every parent, so merges that kept one side don't count.
        if id.is_some() && commit.parents().all(|parent| file_id(&parent) != id) {
            commits.push(commit_info(&commit, you.as_deref()));
            if commits.len() == limit {
                break;
            }
        }
    }
    Ok(commits)
}

/// The newest `limit` commits of every branch, remote branch and HEAD, children before parents.
fn git_graph(workspace: &Path, limit: usize) -> Result<Vec<GraphCommit>> {
    let repo = Repository::discover(workspace)?;
    let you = repo.config()?.get_string("user.email").ok();
    let mut walk = repo.revwalk()?;
    walk.set_sorting(git2::Sort::TOPOLOGICAL | git2::Sort::TIME)?;
    let head = repo.head().ok();
    let head_branch = head
        .as_ref()
        .filter(|h| h.is_branch())
        .and_then(|h| h.shorthand().ok());
    let mut refs: HashMap<Oid, Vec<String>> = HashMap::new();
    if let Some(id) = head.as_ref().and_then(|h| h.target()) {
        walk.push(id)?;
        if head_branch.is_none() {
            refs.entry(id).or_default().push("HEAD".to_string());
        }
    }
    for reference in repo.references()?.flatten() {
        let (Ok(name), Ok(commit)) =
            (reference.shorthand(), reference.peel_to_commit())
        else {
            continue;
        };
        let label = if reference.is_tag() {
            format!("tag: {name}")
        } else if reference.is_branch() || reference.is_remote() {
            // `origin/HEAD` only repeats the remote's default branch.
            if reference.target().is_none() {
                continue;
            }
            walk.push(commit.id())?;
            if Some(name) == head_branch {
                format!("HEAD -> {name}")
            } else {
                name.to_string()
            }
        } else {
            continue;
        };
        let labels = refs.entry(commit.id()).or_default();
        if label.starts_with("HEAD") {
            labels.insert(0, label);
        } else {
            labels.push(label);
        }
    }
    walk.take(limit)
        .map(|id| {
            let commit = repo.find_commit(id?)?;
            Ok(GraphCommit {
                info: commit_info(&commit, you.as_deref()),
                parents: commit.parent_ids().map(|id| id.to_string()).collect(),
                refs: refs.remove(&commit.id()).unwrap_or_default(),
            })
        })
        .collect()
}

fn commit_info(commit: &git2::Commit, you: Option<&str>) -> CommitInfo {
    let author = commit.author();
    let is_you = you
        .zip(author.email().ok())
        .is_some_and(|(you, email)| you.eq_ignore_ascii_case(email));
    CommitInfo {
        id: commit.id().to_string(),
        author: if is_you {
            "You"
        } else {
            author.name().unwrap_or_default()
        }
        .to_string(),
        time: author.when().seconds(),
        summary: commit
            .summary()
            .ok()
            .flatten()
            .unwrap_or_default()
            .to_string(),
    }
}

/// Blame of `content`, the open buffer of `path`: lines it changed since HEAD are uncommitted.
// shortcut: re-blames the whole history on every call (0.4s on a 3700 commit file);
// cache the committed blame per HEAD if refreshes after edits feel slow.
fn git_blame(path: &Path, content: &str) -> Result<Vec<BlameHunk>> {
    let repo = Repository::discover(path.parent().unwrap_or(path))?;
    let workdir = repo.workdir().ok_or_else(|| anyhow!("bare repository"))?;
    let committed = repo.blame_file(path.strip_prefix(workdir)?, None)?;
    let blame = committed.blame_buffer(content.as_bytes())?;
    let you = repo.config()?.get_string("user.email").ok();
    let mut hunks = Vec::with_capacity(blame.len());
    for hunk in blame.iter() {
        let id = hunk.final_commit_id();
        // Hunks split around buffer edits lose their signature, so read the commit.
        let commit = if id.is_zero() {
            None
        } else {
            Some(commit_info(&repo.find_commit(id)?, you.as_deref()))
        };
        hunks.push(BlameHunk {
            start_line: hunk.final_start_line().saturating_sub(1),
            line_count: hunk.lines_in_hunk(),
            commit,
        });
    }
    Ok(hunks)
}

fn git_get_remote_file_url(workspace_path: &Path, file: &Path) -> Result<String> {
    let repo = Repository::discover(workspace_path)?;
    let head = repo.head()?;
    let target_remote = repo.find_remote(
        repo.branch_upstream_remote(head.name().unwrap())?
            .as_str()
            .unwrap(),
    )?;

    // Grab URL part of remote
    let remote = target_remote
        .url()
        .map_err(|e| anyhow!("Failed to convert remote to str: {e}"))?;

    let remote_url = match Url::parse(remote) {
        Ok(url) => url,
        Err(_) => {
            // Parse URL as ssh
            Url::parse(&format!("ssh://{}", remote.replacen(':', "/", 1)))?
        }
    };

    // Get host part
    let host = remote_url
        .host_str()
        .ok_or(anyhow!("Couldn't find remote host"))?;
    // Get namespace (e.g. organisation/project in case of GitHub, org/team/team/team/../project on GitLab)
    let namespace = if let Some(stripped) = remote_url.path().strip_suffix(".git") {
        stripped
    } else {
        remote_url.path()
    };

    let commit = head.peel_to_commit()?.id();

    let file_path = file
        .strip_prefix(workspace_path)?
        .to_str()
        .ok_or(anyhow!("Couldn't convert file path to str"))?;

    let url = format!("https://{host}{namespace}/blob/{commit}/{file_path}",);

    Ok(url)
}

fn search_in_path(
    id: u64,
    current_id: &AtomicU64,
    paths: impl Iterator<Item = PathBuf>,
    pattern: &str,
    case_sensitive: bool,
    whole_word: bool,
    is_regex: bool,
) -> Result<ProxyResponse, RpcError> {
    let mut matches = IndexMap::new();
    let mut matcher = RegexMatcherBuilder::new();
    let matcher = matcher.case_insensitive(!case_sensitive).word(whole_word);
    let matcher = if is_regex {
        matcher.build(pattern)
    } else {
        matcher.build_literals(&[&regex::escape(pattern)])
    };
    let matcher = matcher.map_err(|_| RpcError {
        code: 0,
        message: "can't build matcher".to_string(),
    })?;
    let mut searcher = SearcherBuilder::new().build();

    for path in paths {
        if current_id.load(Ordering::SeqCst) != id {
            return Err(RpcError {
                code: 0,
                message: "expired search job".to_string(),
            });
        }

        if path.is_file() {
            let mut line_matches = Vec::new();
            if let Err(err) = searcher.search_path(
                &matcher,
                path.clone(),
                UTF8(|lnum, line| {
                    if current_id.load(Ordering::SeqCst) != id {
                        return Ok(false);
                    }

                    let mymatch = matcher.find(line.as_bytes())?.unwrap();
                    let line = if line.len() > 200 {
                        // Shorten the line to avoid sending over absurdly long-lines
                        // (such as in minified javascript)
                        // Note that the start/end are column based, not absolute from the
                        // start of the file.
                        let left_keep = line[..mymatch.start()]
                            .chars()
                            .rev()
                            .take(100)
                            .map(|c| c.len_utf8())
                            .sum::<usize>();
                        let right_keep = line[mymatch.end()..]
                            .chars()
                            .take(100)
                            .map(|c| c.len_utf8())
                            .sum::<usize>();
                        let display_range =
                            mymatch.start() - left_keep..mymatch.end() + right_keep;
                        line[display_range].to_string()
                    } else {
                        line.to_string()
                    };
                    line_matches.push(SearchMatch {
                        line: lnum as usize,
                        start: mymatch.start(),
                        end: mymatch.end(),
                        line_content: line,
                    });
                    Ok(true)
                }),
            ) {
                {
                    tracing::error!("{:?}", err);
                }
            }
            if !line_matches.is_empty() {
                matches.insert(path.clone(), line_matches);
            }
        }
    }

    Ok(ProxyResponse::GlobalSearchResponse { matches })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Commits `content` as notes.txt in a repository at `dir`, created on first use.
    fn commit_notes(dir: &Path, content: &str, message: &str) -> Oid {
        let repo = Repository::open(dir).unwrap_or_else(|_| {
            let repo = Repository::init(dir).unwrap();
            repo.config()
                .unwrap()
                .set_str("user.email", "ada@example.com")
                .unwrap();
            repo
        });
        fs::write(dir.join("notes.txt"), content).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("notes.txt")).unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let ada = git2::Signature::now("Ada", "ada@example.com").unwrap();
        let parent = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
        let parents: Vec<_> = parent.iter().collect();
        repo.commit(Some("HEAD"), &ada, &ada, message, &tree, &parents)
            .unwrap()
    }

    fn temp_repo_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("planck-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn git_blame_marks_buffer_edits_uncommitted() {
        let dir = temp_repo_dir("blame");
        commit_notes(&dir, "one\ntwo\n", "add notes\n\nbody");

        let hunks = git_blame(&dir.join("notes.txt"), "one\nnew\ntwo\n");
        fs::remove_dir_all(&dir).unwrap();
        let hunks = hunks.unwrap();
        let line = |line: usize| {
            let hunk = hunks
                .iter()
                .find(|h| {
                    (h.start_line..h.start_line + h.line_count).contains(&line)
                })
                .unwrap();
            hunk.commit
                .as_ref()
                .map(|c| (c.author.as_str(), c.summary.as_str()))
        };
        assert_eq!(line(0), Some(("You", "add notes")));
        assert_eq!(line(1), None);
        assert_eq!(line(2), Some(("You", "add notes")));
    }

    #[test]
    fn git_graph_lists_branches_with_parents_and_refs() {
        let dir = temp_repo_dir("graph");
        let first = commit_notes(&dir, "one\n", "first");
        let repo = Repository::open(&dir).unwrap();
        repo.branch("feature", &repo.find_commit(first).unwrap(), false)
            .unwrap();
        repo.tag_lightweight("v1", &repo.find_object(first, None).unwrap(), false)
            .unwrap();
        let second = commit_notes(&dir, "two\n", "second");

        let commits = git_graph(&dir, 10);
        // master or main, depending on the user's git config.
        let head = repo.head().unwrap().shorthand().unwrap().to_string();
        fs::remove_dir_all(&dir).unwrap();
        let commits = commits.unwrap();
        let rows: Vec<_> = commits
            .iter()
            .map(|c| (c.info.summary.as_str(), c.parents.clone(), c.refs.clone()))
            .collect();
        assert_eq!(
            rows,
            [
                (
                    "second",
                    vec![first.to_string()],
                    vec![format!("HEAD -> {head}")]
                ),
                (
                    "first",
                    vec![],
                    vec!["feature".to_string(), "tag: v1".to_string()]
                ),
            ]
        );
        assert_eq!(commits[0].info.id, second.to_string());
    }

    #[test]
    fn file_versions_and_commits() {
        let dir = temp_repo_dir("versions");
        let first = commit_notes(&dir, "one\n", "add notes");
        commit_notes(&dir, "one\ntwo\n", "add two");
        let path = dir.join("notes.txt");

        let head = file_at_version(&path, "head");
        let old = file_at_version(&path, &first.to_string());
        let parent = file_at_version(&path, "HEAD~1");
        let commits = git_file_commits(&path, 10);
        fs::remove_dir_all(&dir).unwrap();

        assert_eq!(head.unwrap(), "one\ntwo\n");
        assert_eq!(old.unwrap(), "one\n");
        assert_eq!(parent.unwrap(), "one\n");
        let commits = commits.unwrap();
        let summaries: Vec<_> = commits.iter().map(|c| c.summary.as_str()).collect();
        assert_eq!(summaries, ["add two", "add notes"]);
        assert_eq!(commits[1].id, first.to_string());
    }
}
