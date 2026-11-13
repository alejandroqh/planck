//! Run configurations: programs launched in a terminal from `run.toml`,
//! the palette or a code lens.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub struct RunProgram {
    pub program: String,
    pub args: Option<Vec<String>>,
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub struct RunConfig {
    pub name: String,
    pub program: String,
    pub args: Option<Vec<String>>,
    pub cwd: Option<String>,
    pub env: Option<HashMap<String, String>>,
    pub prelaunch: Option<RunProgram>,
    #[serde(default)]
    pub config_source: ConfigSource,
}

#[derive(Deserialize, Serialize, Debug, Clone, Default, PartialEq, Eq)]
pub enum ConfigSource {
    #[default]
    Palette,
    RunInTerminal,
    CodeLens,
}

impl ConfigSource {
    pub fn from_palette(&self) -> bool {
        *self == Self::Palette
    }
}
