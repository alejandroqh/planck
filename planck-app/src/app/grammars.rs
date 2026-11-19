use std::{fs, path::Path};

use anyhow::{Result, anyhow};
use planck_core::directory::Directory;

// Built into Planck by build.rs, which says where they come from.
const RELEASE: &str = env!("PLANCK_GRAMMARS_RELEASE");
const GRAMMARS: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/grammars.tar.zst"));
const QUERIES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/queries.tar.zst"));

/// Unpacks the grammars and queries built into Planck, unless this release of
/// them is already on disk. Returns whether anything was unpacked.
pub fn unpack_grammars() -> Result<bool> {
    let grammars = Directory::grammars_directory()
        .ok_or_else(|| anyhow!("can't get grammars directory"))?;
    let queries = Directory::queries_directory()
        .ok_or_else(|| anyhow!("can't get queries directory"))?;
    Ok(unpack(GRAMMARS, &grammars)? | unpack(QUERIES, &queries)?)
}

fn unpack(archive: &[u8], dir: &Path) -> Result<bool> {
    let version = dir.join("version");
    if fs::read_to_string(&version).is_ok_and(|v| v == RELEASE) {
        return Ok(false);
    }
    tar::Archive::new(zstd::stream::read::Decoder::new(archive)?).unpack(dir)?;
    fs::write(version, RELEASE)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The built-in queries unpack, and only once per release.
    #[test]
    fn unpacks_once() {
        let dir = std::env::temp_dir()
            .join(format!("planck-queries-test-{}", std::process::id()));
        assert!(unpack(QUERIES, &dir).unwrap());
        assert!(dir.join("rust").join("highlights.scm").exists());
        assert!(!unpack(QUERIES, &dir).unwrap());
        fs::remove_dir_all(&dir).unwrap();
    }
}
