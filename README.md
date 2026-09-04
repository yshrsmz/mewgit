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

## License

MIT OR Apache-2.0
