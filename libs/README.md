# Vendored libraries

Libraries Planck used to pull from the Lapce GitHub organization, copied here so
Planck builds without depending on those repositories and so they can be fixed
alongside Planck. Each keeps its upstream license. They are excluded from the
Planck workspace, so `cargo fmt` and `cargo clippy` in the repo root skip them.

| Directory    | Upstream                                | Commit                                     | License |
|--------------|-----------------------------------------|--------------------------------------------|---------|
| `floem`      | https://github.com/lapce/floem          | `31fa8f444c37f4c314f47d88c23ffdbc25f2ab53` | MIT     |
| `vger-rs`    | https://github.com/lapce/vger-rs        | `3206d47ec1e30b645ddcb0687036ba3e1f0d98ec` | MIT     |
| `lsp-types`  | https://github.com/lapce/lsp-types      | `feaa1e2ec80975c9dadd400a238ceacf071058e6` | MIT     |
| `structdesc` | https://github.com/lapce/structdesc     | `bb56969f22fdb2c2d6c03f158fd4a2bdc983b659` | MIT     |

Changes from upstream:
- `floem`: dropped `examples/`, `docs/` and the `vello` renderer; its `vger`
  renderer points at `../../vger-rs` instead of the git repository.

To take an upstream fix, diff the upstream repository from the commit above and
apply the change here, then update the commit in this table.
