Name:           planck-git
Version:        1.2.2.{{{ git_dir_version }}}
Release:        1
Summary:        Fast, focused code editor
License:        Apache-2.0
URL:            https://github.com/alejandroqh/planck

VCS:            {{{ git_dir_vcs }}}
Source:        	{{{ git_dir_pack }}}

BuildRequires:  cargo libxkbcommon-x11-devel libxcb-devel vulkan-loader-devel wayland-devel pkgconf libxkbcommon-x11-devel

%description
Planck is written in pure Rust, with a UI in Floem (also written in Rust).
It is designed with Rope Science from the Xi-Editor, enabling lightning-fast computation, and leverages wgpu for rendering.

%prep
{{{ git_dir_setup_macro }}}
cargo fetch --locked

%build
cargo build --profile release-lto --package planck-app --frozen

%install
install -Dm755 target/release-lto/planck %{buildroot}%{_bindir}/planck
install -Dm644 extra/linux/io.github.alejandroqh.planck.desktop %{buildroot}/usr/share/applications/io.github.alejandroqh.planck.desktop
install -Dm644 extra/linux/io.github.alejandroqh.planck.metainfo.xml %{buildroot}/usr/share/metainfo/io.github.alejandroqh.planck.metainfo.xml
install -Dm644 extra/linux/io.github.alejandroqh.planck.svg %{buildroot}/usr/share/icons/hicolor/scalable/apps/io.github.alejandroqh.planck.svg

%files
%license LICENSE*
%doc *.md
%{_bindir}/planck
/usr/share/applications/io.github.alejandroqh.planck.desktop
/usr/share/metainfo/io.github.alejandroqh.planck.metainfo.xml
/usr/share/icons/hicolor/scalable/apps/io.github.alejandroqh.planck.svg

%changelog
* Mon Jan 01 2024 Jakub Panek
- See full changelog on GitHub
