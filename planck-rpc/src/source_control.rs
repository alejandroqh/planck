use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct DiffInfo {
    pub head: String,
    pub branches: Vec<String>,
    pub tags: Vec<String>,
    pub diffs: Vec<FileDiff>,
    /// Hash of every ref and its target, so a commit, fetch or checkout sends a new DiffInfo.
    pub refs_hash: u64,
}

/// Lines of a buffer last changed by the same commit, or not committed yet.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BlameHunk {
    /// First line, 0 based.
    pub start_line: usize,
    pub line_count: usize,
    /// None for uncommitted lines.
    pub commit: Option<CommitInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct CommitInfo {
    pub id: String,
    /// "You" when the author is the repository's user.
    pub author: String,
    /// Author time in seconds since the Unix epoch.
    pub time: i64,
    pub summary: String,
}

/// A commit of the Git Graph panel.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct GraphCommit {
    pub info: CommitInfo,
    pub parents: Vec<String>,
    /// Branches and tags pointing at it, like `git log --decorate`: "HEAD -> master", "origin/master", "tag: v1.0".
    pub refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum FileDiff {
    Modified(PathBuf),
    Added(PathBuf),
    Deleted(PathBuf),
    Renamed(PathBuf, PathBuf),
}

impl FileDiff {
    pub fn path(&self) -> &PathBuf {
        match &self {
            FileDiff::Modified(p)
            | FileDiff::Added(p)
            | FileDiff::Deleted(p)
            | FileDiff::Renamed(_, p) => p,
        }
    }

    pub fn kind(&self) -> FileDiffKind {
        match self {
            FileDiff::Modified(_) => FileDiffKind::Modified,
            FileDiff::Added(_) => FileDiffKind::Added,
            FileDiff::Deleted(_) => FileDiffKind::Deleted,
            FileDiff::Renamed(_, _) => FileDiffKind::Renamed,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileDiffKind {
    Modified,
    Added,
    Deleted,
    Renamed,
}
