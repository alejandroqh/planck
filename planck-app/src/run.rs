use std::time::Instant;

use planck_rpc::run::RunConfig;
use serde::{Deserialize, Serialize};

/// A program started from a run configuration, shown in a terminal tab.
#[derive(Clone)]
pub struct RunProcess {
    pub config: RunConfig,
    pub stopped: bool,
    pub created: Instant,
    pub is_prelaunch: bool,
}

/// The contents of `.planck/run.toml`.
#[derive(Deserialize, Serialize)]
pub struct RunConfigs {
    pub configs: Vec<RunConfig>,
}
