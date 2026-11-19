//! Builds the tree-sitter grammars and highlight queries into Planck, so the app
//! never downloads them. They come from Planck's copy of a
//! lapce/tree-sitter-grammars release (made by .github/workflows/grammars.yml),
//! fetched once per build directory and checked against the hashes below.

use std::{env, fs, path::Path, process::Command};

use sha2::{Digest, Sha256};

/// The release .github/workflows/grammars.yml made. A new one needs new hashes.
const RELEASE: &str = "grammars-v0.4.5";

/// The sha256 of each file of `RELEASE`, as GitHub lists it.
const ASSETS: &[(&str, &str)] = &[
    (
        "grammars-linux-aarch64.tar.zst",
        "665b0d5bca8ab7438a430377e5cdf01c098a99a4608130c223f0d0438e5d70b5",
    ),
    (
        "grammars-linux-x86_64.tar.zst",
        "e4a21c43801e53b20c080532d9d21227e113f3e525b9cc72ae03ad1e715bc955",
    ),
    (
        "grammars-macos-aarch64.tar.zst",
        "0b5263de53be0883bdc60fcbc2a9cabcef90c8b58466c97eacf687cc3673eedc",
    ),
    (
        "grammars-macos-x86_64.tar.zst",
        "0d02257b9fdb137fdcde6d2ca5e0d6a30267f33ad1d622f0f5291082bea48b92",
    ),
    (
        "grammars-windows-x86_64.tar.zst",
        "22ecf7ee257f48a04392c39ec6949ad7815d693633e799f75c957595922ed9a0",
    ),
    (
        "queries.tar.zst",
        "ce9364104fe91b9eb9b5963330b386d762cf1404e1d74ac32c0ca99965566806",
    ),
];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rustc-env=PLANCK_GRAMMARS_RELEASE={RELEASE}");

    let os = env::var("CARGO_CFG_TARGET_OS").unwrap();
    let arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    let out = env::var("OUT_DIR").unwrap();
    let out = Path::new(&out);
    fetch(
        &format!("grammars-{os}-{arch}.tar.zst"),
        &out.join("grammars.tar.zst"),
    );
    fetch("queries.tar.zst", &out.join("queries.tar.zst"));
}

fn fetch(name: &str, dest: &Path) {
    let hash = ASSETS
        .iter()
        .find(|(n, _)| *n == name)
        .unwrap_or_else(|| panic!("{RELEASE} has no {name} for this platform"))
        .1;
    if sha256(dest).as_deref() == Some(hash) {
        return;
    }

    let url = format!(
        "https://github.com/alejandroqh/planck/releases/download/{RELEASE}/{name}"
    );
    let status = Command::new("curl")
        .args(["-fsSL", "--retry", "3", "-o"])
        .arg(dest)
        .arg(&url)
        .status()
        .expect("curl is needed to fetch the grammars");
    assert!(status.success(), "failed to download {url}");

    let got = sha256(dest);
    if got.as_deref() != Some(hash) {
        let _ = fs::remove_file(dest);
        panic!("{url} has sha256 {got:?}, expected {hash}");
    }
}

fn sha256(path: &Path) -> Option<String> {
    Some(format!("{:x}", Sha256::digest(fs::read(path).ok()?)))
}
