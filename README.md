# mewgit

A TUI git client with a tig-style commit graph and GitHub (incl. EMU) integration.
Authentication is delegated entirely to your local `~/.ssh` and `gh` CLI.

- Commit graph, commit details and diffs
- Staging (files and hunks), commit, branches, stash
- fetch / pull / push through the `git` CLI, so your ssh config and hooks apply
- Interactive rebase, cherry-pick, revert, with continue / skip / abort
- Pull requests of the current repository: list, checks, create, checkout

Status: 0.1, used by the author. See `docs/requirements.md` for the scope and
`docs/plan.md` for what is left.

## Requirements

- macOS or Linux
- `git` 2.38.0 or later. mewgit refuses to start with an older one
- `gh` 2.81.0 or later, logged in (`gh auth login`), for the GitHub features.
  Without it, the PR pane is disabled and the git features still work

## Install

### Homebrew

macOS and Linux on arm64. Homebrew loads formulae from third-party taps only after you trust them:

```sh
brew tap yshrsmz/tap
brew trust --formula yshrsmz/tap/mewgit
brew install yshrsmz/tap/mewgit
```

### From a release

Binaries are attached to each [GitHub Release](https://github.com/yshrsmz/mewgit/releases) for `aarch64-apple-darwin` and `aarch64-unknown-linux-gnu`:

```sh
gh release download -R yshrsmz/mewgit --pattern '*aarch64-apple-darwin.tar.gz*'
shasum -a 256 -c mewgit-*.tar.gz.sha256
tar -xzf mewgit-*.tar.gz
```

Then put `mewgit-<version>-<target>/mewgit` somewhere on your `PATH`.

### From source

Requires [rustup](https://rustup.rs). The toolchain version is pinned in
`rust-toolchain.toml`, and rustup switches to it automatically.

The package is named `mewgit`, but it lives in `crates/mewgit-tui`.

```sh
cargo install --path crates/mewgit-tui --locked
```

This builds a release binary into `~/.cargo/bin/mewgit`. `--locked` builds
with the versions pinned in `Cargo.lock`, the same as CI. To update, pull and
run the same command again; it overwrites the installed binary.

To uninstall:

```sh
cargo uninstall mewgit
```

## Usage

```sh
mewgit [options] [--] [repo-path]
```

Without a path, it opens the repository in the current directory. Press `?`
for the keys of the focused pane, and `q` to quit.

`mewgit --version` prints the version, and `mewgit --help` the usage. Options
may come anywhere; put a path that starts with `-` after `--`.

## Build

```sh
cargo build --workspace
cargo run -p mewgit -- <repo-path>
```

## Releasing

[release-please](https://github.com/googleapis/release-please) keeps a release PR open on `main`, built from the Conventional Commits since the last release. Merging it tags `vX.Y.Z` and creates a draft GitHub Release. The `Release` workflow attaches the binaries and the Homebrew formula with build provenance attestations, publishes the release, and notifies [yshrsmz/homebrew-tap](https://github.com/yshrsmz/homebrew-tap), which updates `Formula/mewgit.rb`. See `.github/workflows/release.yml`.

## License

MIT OR Apache-2.0
