use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use planck_core::{directory::Directory, meta};
use serde::Deserialize;

#[derive(Clone, Deserialize, Debug)]
pub struct ReleaseInfo {
    pub tag_name: String,
    pub target_commitish: String,
    pub assets: Vec<ReleaseAsset>,
    #[serde(skip)]
    pub version: String,
}

#[derive(Clone, Deserialize, Debug)]
pub struct ReleaseAsset {
    pub name: String,
    pub browser_download_url: String,
}

pub fn get_latest_release() -> Result<ReleaseInfo> {
    let url = match meta::RELEASE {
        meta::ReleaseType::Debug => {
            return Err(anyhow!("no release for debug"));
        }
        meta::ReleaseType::Nightly => {
            "https://api.github.com/repos/alejandroqh/planck/releases/tags/nightly"
        }
        _ => "https://api.github.com/repos/alejandroqh/planck/releases/latest",
    };

    let resp = planck_proxy::get_url(url, Some("Planck"))?;
    if !resp.status().is_success() {
        return Err(anyhow!("get release info failed {}", resp.text()?));
    }
    let mut release: ReleaseInfo = serde_json::from_str(&resp.text()?)?;

    release.version = match release.tag_name.as_str() {
        "nightly" => format!(
            "{}+Nightly.{}",
            env!("CARGO_PKG_VERSION"),
            &release.target_commitish[..7]
        ),
        _ => release
            .tag_name
            .strip_prefix('v')
            .unwrap_or(&release.tag_name)
            .to_owned(),
    };

    Ok(release)
}

impl ReleaseInfo {
    /// The file this platform installs. A release can be published before CI has
    /// attached every build, so it may be missing.
    pub fn platform_asset(&self) -> Option<&ReleaseAsset> {
        let name = platform_asset_name().ok()?;
        self.assets.iter().find(|asset| asset.name == name)
    }
}

/// The name of the release file the updater installs on this platform.
fn platform_asset_name() -> Result<&'static str> {
    Ok(match std::env::consts::OS {
        "macos" => "Planck-macos.dmg",
        "linux" => match std::env::consts::ARCH {
            "aarch64" => "planck-linux-arm64.tar.gz",
            "x86_64" => "planck-linux-amd64.tar.gz",
            _ => return Err(anyhow!("arch not supported")),
        },
        #[cfg(feature = "portable")]
        "windows" => "Planck-windows-portable.zip",
        #[cfg(not(feature = "portable"))]
        "windows" => "Planck-windows.msi",
        _ => return Err(anyhow!("os not supported")),
    })
}

pub fn download_release(release: &ReleaseInfo) -> Result<PathBuf> {
    let dir =
        Directory::updates_directory().ok_or_else(|| anyhow!("no directory"))?;
    let name = platform_asset_name()?;
    let asset = release.platform_asset().ok_or_else(|| {
        anyhow!("{} has no {name} to download yet", release.tag_name)
    })?;
    let file_path = dir.join(name);

    let mut resp = planck_proxy::get_url(&asset.browser_download_url, None)?;
    if !resp.status().is_success() {
        return Err(anyhow!("download file error {}", resp.text()?));
    }
    let mut out = std::fs::File::create(&file_path)?;
    resp.copy_to(&mut out)?;
    Ok(file_path)
}

#[cfg(target_os = "macos")]
pub fn extract(src: &Path, process_path: &Path) -> Result<PathBuf> {
    let info = dmg::Attach::new(src).with()?;
    let dest = process_path.parent().ok_or_else(|| anyhow!("no parent"))?;
    let dest = if dest.file_name().and_then(|s| s.to_str()) == Some("MacOS") {
        dest.parent().unwrap().parent().unwrap().parent().unwrap()
    } else {
        dest
    };
    std::fs::remove_dir_all(dest.join("Planck.app"))?;
    fs_extra::copy_items(
        &[info.mount_point.join("Planck.app")],
        dest,
        &fs_extra::dir::CopyOptions {
            overwrite: true,
            skip_exist: false,
            buffer_size: 64000,
            copy_inside: true,
            content_only: false,
            depth: 0,
        },
    )?;
    Ok(dest.join("Planck.app"))
}

#[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "openbsd"))]
pub fn extract(src: &Path, process_path: &Path) -> Result<PathBuf> {
    let tar_gz = std::fs::File::open(src)?;
    let tar = flate2::read::GzDecoder::new(tar_gz);
    let mut archive = tar::Archive::new(tar);
    let parent = src.parent().ok_or_else(|| anyhow::anyhow!("no parent"))?;
    archive.unpack(parent)?;
    std::fs::remove_file(process_path)?;
    std::fs::copy(parent.join("Planck").join("planck"), process_path)?;
    Ok(process_path.to_path_buf())
}

#[cfg(all(target_os = "windows", feature = "portable"))]
pub fn extract(src: &Path, process_path: &Path) -> Result<PathBuf> {
    let parent = src
        .parent()
        .ok_or_else(|| anyhow::anyhow!("src has no parent"))?;
    let dst_parent = process_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("process_path has no parent"))?;

    {
        let mut archive = zip::ZipArchive::new(std::fs::File::open(src)?)?;
        archive.extract(parent)?;
    }

    // TODO(dbuga): instead of replacing the exe, run the msi installer for non-portable
    // TODO(dbuga): there's a very slight chance the user might end up with a backup file without a working .exe
    std::fs::rename(process_path, dst_parent.join("planck.exe.bak"))?;
    std::fs::copy(parent.join("planck.exe"), process_path)?;

    Ok(process_path.to_path_buf())
}

#[cfg(all(target_os = "windows", not(feature = "portable")))]
pub fn extract(src: &Path, _process_path: &Path) -> Result<PathBuf> {
    // We downloaded an uncompressed msi installer, nothing to extract.
    // On the other hand, we need to run this msi so pass its path back out.
    Ok(src.to_path_buf())
}

#[cfg(target_os = "macos")]
pub fn restart(path: &Path) -> Result<()> {
    use std::os::unix::process::CommandExt;
    // exec only returns if it failed.
    let err = std::process::Command::new("open")
        .arg("-n")
        .arg(path)
        .arg("--args")
        .arg("-n")
        .exec();
    Err(anyhow!("can't relaunch Planck: {err}"))
}

#[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "openbsd"))]
pub fn restart(path: &Path) -> Result<()> {
    use std::os::unix::process::CommandExt;
    // exec only returns if it failed.
    let err = std::process::Command::new(path).arg("-n").exec();
    Err(anyhow!("can't relaunch Planck: {err}"))
}

#[cfg(all(target_os = "windows", feature = "portable"))]
pub fn restart(path: &Path) -> Result<()> {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x00000008;
    let process_id = std::process::id();
    let path = path
        .to_str()
        .ok_or_else(|| anyhow!("can't get path to str"))?;
    std::process::Command::new("cmd")
        .raw_arg(format!(
            r#"/C taskkill /PID {process_id} & start "" "{path}""#
        ))
        .creation_flags(DETACHED_PROCESS)
        .spawn()?;
    Ok(())
}

#[cfg(all(target_os = "windows", not(feature = "portable")))]
pub fn restart(path: &Path) -> Result<()> {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x00000008;
    let process_id = std::process::id();
    let path = path
        .to_str()
        .ok_or_else(|| anyhow!("can't get path to str"))?;

    let planck_exe = std::env::current_exe()
        .map_err(|err| anyhow!("can't get path to exe").context(err))?;
    let planck_exe = planck_exe
        .to_str()
        .ok_or_else(|| anyhow!("can't convert exe path to str"))?;

    std::process::Command::new("cmd")
        .raw_arg(format!(
            r#"/C taskkill /PID {process_id} & msiexec /i "{path}" /qb & start "" "{planck_exe}""#,
        ))
        .creation_flags(DETACHED_PROCESS)
        .spawn()?;
    Ok(())
}

#[cfg(all(target_os = "windows", feature = "portable"))]
pub fn cleanup() {
    // Clean up backup exe after an update
    if let Ok(process_path) = std::env::current_exe() {
        if let Some(dst_parent) = process_path.parent() {
            if let Err(err) = std::fs::remove_file(dst_parent.join("planck.exe.bak"))
            {
                tracing::error!("{:?}", err);
            }
        }
    }
}

#[cfg(any(
    not(target_os = "windows"),
    all(target_os = "windows", not(feature = "portable"))
))]
pub fn cleanup() {
    // Nothing to do yet
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(assets: &[&str]) -> ReleaseInfo {
        ReleaseInfo {
            tag_name: "v1.0.9".to_string(),
            target_commitish: "master".to_string(),
            assets: assets
                .iter()
                .map(|name| ReleaseAsset {
                    name: name.to_string(),
                    browser_download_url: format!("https://example.com/{name}"),
                })
                .collect(),
            version: "1.0.9".to_string(),
        }
    }

    /// A release published before CI attached this platform's build has nothing to install.
    #[test]
    fn platform_asset_needs_this_platforms_file() {
        let Ok(name) = platform_asset_name() else {
            return;
        };
        let linux_only =
            release(&["planck_1.0.9-1_amd64.deb", "planck-1.0.9-1.x86_64.rpm"]);
        assert!(linux_only.platform_asset().is_none());

        let complete = release(&["planck_1.0.9-1_amd64.deb", name]);
        let asset = complete.platform_asset().unwrap();
        assert_eq!(asset.name, name);
        assert_eq!(
            asset.browser_download_url,
            format!("https://example.com/{name}")
        );
    }
}
