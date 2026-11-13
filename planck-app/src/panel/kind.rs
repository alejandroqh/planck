use serde::{Deserialize, Serialize};
use strum_macros::EnumIter;

use super::{data::PanelOrder, position::PanelPosition};
use crate::config::icon::PlanckIcons;

#[derive(
    Clone, Copy, PartialEq, Serialize, Deserialize, Hash, Eq, Debug, EnumIter,
)]
pub enum PanelKind {
    Terminal,
    FileExplorer,
    SourceControl,
    Search,
    Problem,
    CallHierarchy,
    DocumentSymbol,
    References,
    Implementation,
}

impl PanelKind {
    pub fn svg_name(&self) -> &'static str {
        match &self {
            PanelKind::Terminal => PlanckIcons::TERMINAL,
            PanelKind::FileExplorer => PlanckIcons::FILE_EXPLORER,
            PanelKind::SourceControl => PlanckIcons::SCM,
            PanelKind::Search => PlanckIcons::SEARCH,
            PanelKind::Problem => PlanckIcons::PROBLEM,
            PanelKind::CallHierarchy => PlanckIcons::TYPE_HIERARCHY,
            PanelKind::DocumentSymbol => PlanckIcons::DOCUMENT_SYMBOL,
            PanelKind::References => PlanckIcons::REFERENCES,
            PanelKind::Implementation => PlanckIcons::IMPLEMENTATION,
        }
    }

    pub fn position(&self, order: &PanelOrder) -> Option<(usize, PanelPosition)> {
        for (pos, panels) in order.iter() {
            let index = panels.iter().position(|k| k == self);
            if let Some(index) = index {
                return Some((index, *pos));
            }
        }
        None
    }

    pub fn default_position(&self) -> PanelPosition {
        match self {
            PanelKind::Terminal => PanelPosition::BottomLeft,
            PanelKind::FileExplorer => PanelPosition::LeftTop,
            PanelKind::SourceControl => PanelPosition::LeftTop,
            PanelKind::Search => PanelPosition::BottomLeft,
            PanelKind::Problem => PanelPosition::BottomLeft,
            PanelKind::CallHierarchy => PanelPosition::BottomLeft,
            PanelKind::DocumentSymbol => PanelPosition::RightTop,
            PanelKind::References => PanelPosition::BottomLeft,
            PanelKind::Implementation => PanelPosition::BottomLeft,
        }
    }
}
