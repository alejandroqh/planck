# Contributing to Planck

Thanks for your interest in Planck. Bug reports, feature requests and pull requests are all welcome.

## Issues

Report bugs and request features through [GitHub issues](https://github.com/alejandroqh/planck/issues). For bugs, include your OS, the Planck version (Help → About) and steps to reproduce.

## Code

1. Build from source following [docs/building-from-source.md](docs/building-from-source.md).
2. Keep changes focused. One topic per pull request.
3. Before opening a pull request, run:

```sh
cargo fmt --all
cargo clippy --workspace
cargo test --workspace
```

## Syntax highlighting

Tree-sitter grammars and highlight queries are loaded at runtime. Language definitions live in `planck-core/src/language.rs`.
