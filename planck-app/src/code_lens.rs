use std::rc::Rc;

use planck_rpc::run::{ConfigSource, RunConfig};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{command::InternalCommand, window_tab::CommonData};

#[derive(Serialize, Deserialize)]
struct CargoArgs {
    #[serde(rename = "cargoArgs")]
    pub cargo_args: Vec<String>,

    #[serde(rename = "cargoExtraArgs")]
    pub cargo_extra_args: Vec<String>,

    #[serde(rename = "executableArgs")]
    pub executable_args: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct RustArgs {
    pub args: CargoArgs,
    pub kind: String,
    pub label: String,
    pub location: lsp_types::LocationLink,
}

#[derive(Clone)]
pub struct CodeLensData {
    common: Rc<CommonData>,
}

impl CodeLensData {
    pub fn new(common: Rc<CommonData>) -> Self {
        Self { common }
    }

    pub fn run(&self, command: &str, args: Vec<Value>) {
        match command {
            "rust-analyzer.runSingle" => {
                if let Some(config) = self.get_rust_command_config(&args) {
                    self.common
                        .internal_command
                        .send(InternalCommand::Run { config });
                }
            }
            _ => {
                tracing::debug!("todo {:}", command);
            }
        }
    }

    fn get_rust_command_config(&self, args: &[Value]) -> Option<RunConfig> {
        if let Some(args) = args.first() {
            let Ok(mut cargo_args) =
                serde_json::from_value::<RustArgs>(args.clone())
            else {
                tracing::error!("serde error");
                return None;
            };
            cargo_args
                .args
                .cargo_args
                .extend(cargo_args.args.cargo_extra_args);
            if !cargo_args.args.executable_args.is_empty() {
                cargo_args.args.cargo_args.push("--".to_string());
                cargo_args
                    .args
                    .cargo_args
                    .extend(cargo_args.args.executable_args);
            }
            Some(RunConfig {
                name: cargo_args.label,
                program: cargo_args.kind,
                args: Some(cargo_args.args.cargo_args),
                cwd: None,
                env: None,
                prelaunch: None,
                config_source: ConfigSource::CodeLens,
            })
        } else {
            tracing::error!("no args");
            None
        }
    }
}
