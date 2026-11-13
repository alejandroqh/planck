use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{main_split::SplitInfo, panel::data::PanelInfo};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanckWorkspace {
    pub path: Option<PathBuf>,
    pub last_open: u64,
}

impl PlanckWorkspace {
    pub fn display(&self) -> Option<String> {
        let path = self.path.as_ref()?;
        Some(
            path.file_name()
                .unwrap_or(path.as_os_str())
                .to_string_lossy()
                .to_string(),
        )
    }
}

impl std::fmt::Display for PlanckWorkspace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.path.as_ref().and_then(|p| p.to_str()).unwrap_or(""))
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct WorkspaceInfo {
    pub split: SplitInfo,
    pub panel: PanelInfo,
}
