//! Language server identity and configuration.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::counter::Counter;

/// Identifies one running language server instance.
#[derive(Eq, PartialEq, Hash, Clone, Copy, Debug, Serialize, Deserialize)]
pub struct PluginId(pub u64);

impl PluginId {
    pub fn next() -> Self {
        static PLUGIN_ID_COUNTER: Counter = Counter::new();
        Self(PLUGIN_ID_COUNTER.next())
    }
}

/// A language server configured under `[lsp.<name>]` in settings.
///
/// ```toml
/// [lsp.rust-analyzer]
/// command = "rust-analyzer"
/// languages = ["rust"]
/// ```
#[derive(Deserialize, Serialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "kebab-case", default)]
pub struct LspServerConfig {
    /// Program to run. Looked up on `PATH` unless it is a path.
    pub command: String,
    pub args: Vec<String>,
    /// LSP language ids (`rust`, `python`, `typescriptreact`, ...) this
    /// server handles. It is started the first time one of them is opened.
    pub languages: Vec<String>,
    /// Extra environment variables for the server process.
    pub env: HashMap<String, String>,
    /// Sent as `initializationOptions` in the `initialize` request.
    pub initialization_options: Option<Value>,
    /// Set to `true` to keep a configured server from starting.
    pub disabled: bool,
}

/// All configured language servers, keyed by name.
pub type LspServers = HashMap<String, LspServerConfig>;
