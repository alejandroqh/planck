use std::path::PathBuf;

use lsp_types::{Range, SymbolKind};
use planck_core::line_ending::LineEnding;
use planck_rpc::run::RunConfig;

use crate::{
    command::{PlanckCommand, PlanckWorkbenchCommand},
    editor::location::EditorLocation,
    workspace::PlanckWorkspace,
};

#[derive(Clone, Debug, PartialEq)]
pub struct PaletteItem {
    pub content: PaletteItemContent,
    pub filter_text: String,
    pub score: u32,
    pub indices: Vec<usize>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PaletteItemContent {
    PaletteHelp {
        cmd: PlanckWorkbenchCommand,
    },
    File {
        path: PathBuf,
        full_path: PathBuf,
    },
    Line {
        line: usize,
        content: String,
    },
    Command {
        cmd: PlanckCommand,
    },
    Workspace {
        workspace: PlanckWorkspace,
    },
    Reference {
        path: PathBuf,
        location: EditorLocation,
    },
    DocumentSymbol {
        kind: SymbolKind,
        name: String,
        range: Range,
        container_name: Option<String>,
    },
    WorkspaceSymbol {
        kind: SymbolKind,
        name: String,
        container_name: Option<String>,
        location: EditorLocation,
    },
    Run {
        config: RunConfig,
    },
    ColorTheme {
        name: String,
    },
    IconTheme {
        name: String,
    },
    Language {
        name: String,
    },
    LineEnding {
        kind: LineEnding,
    },
    SCMReference {
        name: String,
    },
    /// A git revision to compare the active file with.
    Revision {
        rev: String,
    },
    TerminalProfile {
        name: String,
        profile: planck_rpc::terminal::TerminalProfile,
    },
}
