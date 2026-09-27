# mewgit

A TUI git client with a tig-style commit graph and GitHub (incl. EMU) integration.
Authentication is delegated entirely to your local `~/.ssh` and `gh` CLI.

Status: design phase. See `docs/requirements.md` and `docs/plan.md`.

## Build

Requires [rustup](https://rustup.rs). The toolchain version is pinned in
`rust-toolchain.toml`, and rustup switches to it automatically.

```sh
cargo build --workspace
cargo run -p mewgit -- <repo-path>
```

## Install from source

The package is named `mewgit`, but it lives in `crates/mewgit-tui`.

```sh
cargo install --path crates/mewgit-tui --locked
mewgit <repo-path>
```

This builds a release binary into `~/.cargo/bin/mewgit`. `--locked` builds
with the versions pinned in `Cargo.lock`, the same as CI. To update, pull and
run the same command again; it overwrites the installed binary.

To uninstall:

```sh
cargo uninstall mewgit
```

## License

MIT OR Apache-2.0
