## Building from source

It is easy to build Planck from source on a GNU/Linux distribution. Cargo handles the build process, all you need to do, is ensure the correct dependencies are installed.

1. Install the Rust compiler and Cargo using [`rustup.rs`](https://rustup.rs/). If you already have the toolchain, ensure you are using latest Rust version.

2. Install dependencies for your operating system:

#### Ubuntu
```sh
sudo apt install clang libxkbcommon-x11-dev pkg-config libvulkan-dev libwayland-dev xorg-dev libxcb-shape0-dev libxcb-xfixes0-dev
```
#### Fedora
```sh
sudo dnf install clang libxkbcommon-x11-devel libxcb-devel vulkan-loader-devel wayland-devel pkgconf
```
#### Void Linux
```sh
sudo xbps-install -S base-devel clang libxkbcommon-devel vulkan-loader wayland-devel
```

3. Clone this repository (this command will clone to your home directory):
```sh
git clone https://github.com/alejandroqh/planck.git ~/planck
```

4. `cd` into the repository, and run the build command with the release flag
```sh
cd ~/planck
```

```sh
cargo install --path . --bin planck --profile release-lto --locked
```

> If you use a different distribution, and are having trouble finding appropriate dependencies, let us know in an issue!

Once Planck is compiled, the executable will be available in `$HOME/.cargo/bin/planck` and should be available in `PATH` automatically.

## Building release packages

Release packages are built by `.github/workflows/release.yml`: a `.deb` and an `.rpm` for amd64 and arm64 on native GitHub runners, and an arm64 `.dmg` on macOS. To build the Linux packages locally:

```bash
cargo install cargo-deb cargo-generate-rpm
cargo build --profile release-lto --package planck-app --no-default-features --features vendored-fonts
cargo deb --no-build --output dist/
cargo generate-rpm --output dist/
```

