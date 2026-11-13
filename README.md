<h1 align="center">Planck</h1>

<h4 align="center">A fast, focused code editor written in Rust</h4>

<div align="center">
  <a href="https://github.com/alejandroqh/planck/actions/workflows/ci.yml" target="_blank">
    <img src="https://github.com/alejandroqh/planck/actions/workflows/ci.yml/badge.svg" />
  </a>
</div>
<br/>

Planck is a code editor written in pure Rust, with a UI in [Floem](https://github.com/lapce/floem). It uses the rope data structure from [Xi-Editor](https://github.com/xi-editor/xi-editor) and renders with [wgpu](https://github.com/gfx-rs/wgpu).

Planck started as a fork of [Lapce](https://github.com/lapce/lapce). It keeps Lapce's editing core and drops the parts it does not need, to stay small and easy to work on.

## Features

* Built-in LSP ([Language Server Protocol](https://microsoft.github.io/language-server-protocol/)) support: point Planck at a language server and get completion, diagnostics and code actions. No plugins needed.
* Modal editing support as a first-class citizen (Vim-like, and toggleable)
* Built-in terminal, so you can run commands in your workspace without leaving Planck

### Differences from Lapce

* No WASI plugin system and no plugin registry. Language servers are configured directly in settings.
* No remote development (SSH / WSL).
* No debugger (DAP).
* Its own config directory. Planck never reads Lapce's settings.

## Installation

Pre-built releases for Windows, Linux and macOS are published on the [releases page](https://github.com/alejandroqh/planck/releases). To compile from source, follow the [guide](docs/building-from-source.md).

## Contributing

See [`CONTRIBUTING.md`](CONTRIBUTING.md).

## License

Planck is released under the Apache License, Version 2.0. See [`LICENSE`](LICENSE).

Planck is derived from [Lapce](https://github.com/lapce/lapce), copyright Dongdong Zhou and the Lapce contributors, also licensed under Apache-2.0.
