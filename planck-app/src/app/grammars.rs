use std::{
    env,
    fs::{self},
    path::PathBuf,
};

use anyhow::{Context, Result, anyhow};
use planck_core::directory::Directory;

use crate::{tracing::*, update::ReleaseInfo};

/// Planck's copy of a github.com/lapce/tree-sitter-grammars release, made by
/// .github/workflows/grammars.yml so Planck only downloads from its own repo.
const GRAMMARS_RELEASE: &str = "grammars-v0.4.5";

fn get_github_api(url: &str) -> Result<String> {
    let user_agent = format!("Planck/{}", planck_core::meta::VERSION);
    let resp = planck_proxy::get_url(url, Some(user_agent.as_str()))?;
    if !resp.status().is_success() {
        return Err(anyhow!("get release info failed {}", resp.text()?));
    }

    Ok(resp.text()?)
}

pub fn find_grammar_release() -> Result<ReleaseInfo> {
    let url = format!(
        "https://api.github.com/repos/alejandroqh/planck/releases/tags/{GRAMMARS_RELEASE}"
    );
    let release = get_github_api(&url)
        .context("Failed to retrieve the tree-sitter grammars release")?;
    Ok(serde_json::from_str(&release)?)
}

pub fn fetch_grammars(release: &ReleaseInfo) -> Result<bool> {
    let dir = Directory::grammars_directory()
        .ok_or_else(|| anyhow!("can't get grammars directory"))?;

    let file_name = format!("grammars-{}-{}", env::consts::OS, env::consts::ARCH);

    let updated = download_release(dir, release, &file_name)?;

    trace!(TraceLevel::INFO, "Successfully downloaded grammars");

    Ok(updated)
}

pub fn fetch_queries(release: &ReleaseInfo) -> Result<bool> {
    let dir = Directory::queries_directory()
        .ok_or_else(|| anyhow!("can't get queries directory"))?;

    let file_name = "queries";

    let updated = download_release(dir, release, file_name)?;

    trace!(TraceLevel::INFO, "Successfully downloaded queries");

    Ok(updated)
}

fn download_release(
    dir: PathBuf,
    release: &ReleaseInfo,
    file_name: &str,
) -> Result<bool> {
    if !dir.exists() {
        fs::create_dir(&dir)?;
    }

    let current_version =
        fs::read_to_string(dir.join("version")).unwrap_or_default();
    let release_version = if release.tag_name == "nightly" {
        format!("nightly-{}", &release.target_commitish[..7])
    } else {
        release.tag_name.clone()
    };

    if release_version == current_version {
        return Ok(false);
    }

    for asset in &release.assets {
        if asset.name.starts_with(file_name) {
            let mut resp = planck_proxy::get_url(&asset.browser_download_url, None)?;
            if !resp.status().is_success() {
                return Err(anyhow!("download file error {}", resp.text()?));
            }

            let file = tempfile::tempfile()?;

            {
                use std::io::{Seek, Write};
                let file = &mut &file;
                resp.copy_to(file)?;
                file.flush()?;
                file.rewind()?;
            }

            if asset.name.ends_with(".zip") {
                let mut archive = zip::ZipArchive::new(file)?;
                archive.extract(&dir)?;
            } else if asset.name.ends_with(".tar.zst") {
                let mut archive =
                    tar::Archive::new(zstd::stream::read::Decoder::new(file)?);
                archive.unpack(&dir)?;
            }

            fs::write(dir.join("version"), &release_version)?;
        }
    }
    Ok(true)
}
