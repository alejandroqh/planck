use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::{main_split::SplitInfo, panel::data::PanelInfo};

pub const SPACE_EXTENSION: &str = "planck-space";

/// One `[[folders]]` entry of a `.planck-space` file.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceFolder {
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Deserialize)]
struct SpaceFile {
    #[serde(default)]
    folders: Vec<SpaceFolder>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanckWorkspace {
    /// The root folder. Inside a space this is the first folder, so code that
    /// only knows about one root keeps working.
    pub path: Option<PathBuf>,
    pub last_open: u64,
    /// The `.planck-space` file this workspace was opened from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space: Option<PathBuf>,
    /// The space's folders as absolute paths, filled by `load_space`.
    #[serde(skip)]
    pub folders: Vec<SpaceFolder>,
}

impl PlanckWorkspace {
    pub fn is_space_file(path: &Path) -> bool {
        path.extension().is_some_and(|e| e == SPACE_EXTENSION)
    }

    /// A workspace for a folder, or for a space when `path` is a `.planck-space` file.
    pub fn from_path(path: PathBuf) -> Self {
        let last_open = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if Self::is_space_file(&path) {
            Self {
                space: Some(path),
                last_open,
                ..Default::default()
            }
        } else {
            Self {
                path: Some(path),
                last_open,
                ..Default::default()
            }
        }
    }

    /// Reads the space file, if any, into `folders` and points `path` at the first folder.
    pub fn load_space(mut self) -> Self {
        let Some(space) = self.space.as_ref() else {
            return self;
        };
        match read_space_folders(space) {
            Ok(folders) => {
                self.path = folders.first().map(|f| f.path.clone());
                self.folders = folders;
            }
            Err(err) => tracing::error!("failed to read {space:?}: {err:?}"),
        }
        self
    }

    /// Every root folder: the space's folders, or the single open folder.
    pub fn folder_paths(&self) -> Vec<PathBuf> {
        if self.space.is_some() {
            self.folders.iter().map(|f| f.path.clone()).collect()
        } else {
            self.path.iter().cloned().collect()
        }
    }

    pub fn display(&self) -> Option<String> {
        if let Some(space) = self.space.as_ref() {
            return Some(space.file_stem()?.to_string_lossy().to_string());
        }
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
        let path = self.space.as_ref().or(self.path.as_ref());
        f.write_str(path.and_then(|p| p.to_str()).unwrap_or(""))
    }
}

/// Folders listed in a space file, relative paths resolved from the file's directory.
pub fn read_space_folders(space: &Path) -> Result<Vec<SpaceFolder>> {
    let file: SpaceFile = toml::from_str(&std::fs::read_to_string(space)?)?;
    let dir = space.parent().unwrap_or(Path::new(""));
    Ok(file
        .folders
        .into_iter()
        .map(|f| {
            let path = dir.join(&f.path);
            SpaceFolder {
                path: std::fs::canonicalize(&path).unwrap_or(path),
                name: f.name,
            }
        })
        .collect())
}

/// The space file's `[settings]` table as TOML text, in the same shape as a settings.toml.
pub fn space_settings(space: &Path) -> Option<String> {
    let parsed = std::fs::read_to_string(space)
        .map_err(anyhow::Error::from)
        .and_then(|s| Ok(toml::from_str::<toml::Value>(&s)?));
    let mut value = match parsed {
        Ok(value) => value,
        Err(err) => {
            tracing::error!("failed to read settings from {space:?}: {err:?}");
            return None;
        }
    };
    let settings = value.as_table_mut()?.remove("settings")?;
    toml::to_string(&settings).ok()
}

/// Appends `folder` to the space file, creating the file if needed. Keeps the
/// rest of the file, comments included.
pub fn add_space_folder(space: &Path, folder: &Path) -> Result<()> {
    let content = match std::fs::read_to_string(space) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    let mut doc: toml_edit::Document = content.parse()?;
    let folders = doc
        .entry("folders")
        .or_insert(toml_edit::Item::ArrayOfTables(Default::default()))
        .as_array_of_tables_mut()
        .ok_or_else(|| anyhow::anyhow!("`folders` is not an array of tables"))?;

    // Folders next to the space file are stored relative so the space can move with them.
    let dir = space.parent().unwrap_or(Path::new(""));
    let stored = match folder.strip_prefix(dir) {
        Ok(p) if p.as_os_str().is_empty() => Path::new("."),
        Ok(p) => p,
        Err(_) => folder,
    };
    let mut table = toml_edit::Table::new();
    table["path"] = toml_edit::value(stored.to_string_lossy().as_ref());
    folders.push(table);

    std::fs::write(space, doc.to_string())?;
    Ok(())
}

#[derive(Clone, Serialize, Deserialize)]
pub struct WorkspaceInfo {
    pub split: SplitInfo,
    pub panel: PanelInfo,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn space_folders_round_trip() {
        let dir = std::env::temp_dir()
            .join(format!("planck-space-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("api")).unwrap();
        let space = dir.join("my.planck-space");
        std::fs::write(&space, "# keep me\n[settings.editor]\ntab-width = 2\n")
            .unwrap();

        add_space_folder(&space, &dir.join("api")).unwrap();
        add_space_folder(&space, Path::new("/elsewhere/web")).unwrap();

        let content = std::fs::read_to_string(&space).unwrap();
        assert!(content.contains("# keep me") && content.contains("tab-width = 2"));
        assert!(content.contains("path = \"api\""));

        let ws = PlanckWorkspace::from_path(space.clone()).load_space();
        assert_eq!(ws.folders.len(), 2);
        assert_eq!(
            ws.path,
            Some(std::fs::canonicalize(dir.join("api")).unwrap())
        );
        assert_eq!(ws.folders[1].path, PathBuf::from("/elsewhere/web"));
        assert_eq!(ws.folder_paths().len(), 2);
        assert_eq!(ws.display().as_deref(), Some("my"));
        assert_eq!(ws.to_string(), space.to_str().unwrap());

        std::fs::remove_dir_all(dir).unwrap();
    }
}
