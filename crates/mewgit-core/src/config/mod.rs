//! `config` モジュール。設計は `docs/design/08-config.md` を参照。
//!
//! 設定の読み込みは失敗しない。問題は [`ConfigWarning`] として返し、既定値で継続する。
//! 呼び出し側は警告を notice として表示する責務を負う。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use globset::{GlobBuilder, GlobMatcher};
use serde::Deserialize;

/// 設定の読み込み中に見つかった問題。エラーではなく警告として扱う。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigWarning {
    /// 設定ファイルは存在するが読み取れなかった（権限など）。
    #[error("設定ファイルを読めませんでした: {path}: {message}")]
    Io { path: PathBuf, message: String },
    /// TOML として解釈できなかった。ファイル全体を捨てて既定値を使う。
    #[error("設定ファイルの構文エラー{}: {message}", DisplaySource(.path))]
    Parse {
        /// 文字列から直接読んだ場合は `None`。
        path: Option<PathBuf>,
        message: String,
    },
    /// `[[repos]]` の `match` が glob として不正。そのルールだけ捨てる。
    #[error("[[repos]] の match が不正な glob です: {pattern}: {message}")]
    InvalidGlob { pattern: String, message: String },
    /// 値が使える範囲から外れている。その項目だけ既定値に戻す。
    #[error("{key} の値 {value} は使えないので {fallback} にしました")]
    OutOfRange {
        key: &'static str,
        value: String,
        fallback: String,
    },
    /// ホームディレクトリが特定できず、設定ファイルの場所を決められなかった。
    #[error("ホームディレクトリを特定できず、設定ファイルを読めませんでした")]
    NoConfigDir,
}

/// `Option<PathBuf>` を `#[error]` に埋め込むためのアダプタ。パスが無ければ何も出さない。
struct DisplaySource<'a>(&'a Option<PathBuf>);

impl std::fmt::Display for DisplaySource<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            Some(path) => write!(f, "（{}）", path.display()),
            None => Ok(()),
        }
    }
}

/// 検証済みの設定。起動時に確定し、実行中は再読込しない。
///
/// `Deserialize` は持たない。生成経路は [`Config::load`] / [`Config::load_from`] /
/// [`Config::from_toml_str`] / [`Config::default`] だけで、いずれも `[[repos]]` の
/// glob をコンパイル済みにする。
#[derive(Debug, Clone, Default)]
pub struct Config {
    pub general: General,
    pub graph: Graph,
    pub github: Github,
    pub repos: Vec<RepoRule>,
    /// アクション名 → キー表記。`KeyEvent` への変換は `mewgit-tui` 側で行う。
    pub keymap: HashMap<String, String>,
    pub theme: Theme,
}

/// TOML 上の形。glob は未コンパイルで、未知のキーは無視する。
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ConfigFile {
    general: General,
    graph: Graph,
    github: Github,
    repos: Vec<RepoRuleSpec>,
    keymap: HashMap<String, String>,
    theme: Theme,
}

/// `[[repos]]` の 1 エントリ（未検証）。
#[derive(Debug, Deserialize)]
struct RepoRuleSpec {
    #[serde(rename = "match")]
    pattern: String,
    user: String,
}

/// `[general]`
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct General {
    pub git_bin: String,
    pub gh_bin: String,
    pub editor_fallback: bool,
    /// status を定期的に読み直す間隔（秒）。外でファイルを編集したのを拾う（08 の
    /// `general.refresh_interval`）。**0 で止める。**
    pub refresh_interval: u64,
}

/// `[graph]`
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Graph {
    pub ascii: bool,
    pub page_size: usize,
    pub colors: Vec<String>,
}

/// `[github]`
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Github {
    pub enabled: bool,
    pub concurrency: usize,
}

/// `[theme]`
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Theme {
    pub graph_head: String,
    pub pr_open: String,
    pub pr_merged: String,
    pub pr_closed: String,
}

/// glob をコンパイル済みの `[[repos]]` エントリ。不正な glob のものは存在しない。
#[derive(Debug, Clone)]
pub struct RepoRule {
    /// `"{host}/{owner}/{repo}"` にマッチさせる glob。
    pub pattern: String,
    /// 使用する GitHub のログイン名。
    pub user: String,
    matcher: GlobMatcher,
}

impl Default for General {
    fn default() -> Self {
        Self {
            git_bin: "git".to_string(),
            gh_bin: "gh".to_string(),
            editor_fallback: false,
            // lazygit の `refresher.refreshInterval` と同じ（01 の「外部の変更の検知」）。
            refresh_interval: 10,
        }
    }
}

impl Default for Graph {
    fn default() -> Self {
        Self {
            ascii: false,
            page_size: 300,
            colors: ["blue", "magenta", "cyan", "green", "yellow", "red"]
                .map(String::from)
                .to_vec(),
        }
    }
}

impl Default for Github {
    fn default() -> Self {
        Self {
            enabled: true,
            concurrency: 4,
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            graph_head: "bold".to_string(),
            pr_open: "green".to_string(),
            pr_merged: "magenta".to_string(),
            pr_closed: "red".to_string(),
        }
    }
}

impl Config {
    /// 既定のパスから読み、環境変数の上書きを適用する。
    ///
    /// ファイルが無い場合は既定値を返し、警告も出さない。
    pub fn load() -> (Config, Vec<ConfigWarning>) {
        let xdg = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from);
        let home = std::env::home_dir();

        let (mut config, warnings) = match resolve_config_path(xdg.as_deref(), home.as_deref()) {
            Some(path) => Self::load_from(&path),
            None => (Config::default(), vec![ConfigWarning::NoConfigDir]),
        };

        config.apply_env(|key| std::env::var(key).ok());
        (config, warnings)
    }

    /// 指定したパスの `config.toml` を読む。存在しなければ既定値（警告なし）。
    ///
    /// パスを外から渡せるようにしているのはテストのため。
    pub fn load_from(path: &Path) -> (Config, Vec<ConfigWarning>) {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text, Some(path)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                (Config::default(), Vec::new())
            }
            Err(err) => (
                Config::default(),
                vec![ConfigWarning::Io {
                    path: path.to_path_buf(),
                    message: err.to_string(),
                }],
            ),
        }
    }

    /// TOML 文字列から読む。パースに失敗したら既定値と [`ConfigWarning::Parse`]。
    pub fn from_toml_str(text: &str) -> (Config, Vec<ConfigWarning>) {
        Self::parse(text, None)
    }

    /// `source` はエラーメッセージに出すファイルパス。文字列から直接読んだなら `None`。
    fn parse(text: &str, source: Option<&Path>) -> (Config, Vec<ConfigWarning>) {
        match toml::from_str::<ConfigFile>(text) {
            Ok(file) => file.into_config(),
            Err(err) => (
                Config::default(),
                vec![ConfigWarning::Parse {
                    path: source.map(Path::to_path_buf),
                    // `to_string` は行・列を含む。`message()` だとそれが落ちる。
                    message: err.to_string(),
                }],
            ),
        }
    }

    /// `MEWGIT_*` による上書きを適用する。`lookup` は環境変数の取得。
    fn apply_env(&mut self, lookup: impl Fn(&str) -> Option<String>) {
        if let Some(value) = lookup("MEWGIT_GIT_BIN").filter(|v| !v.is_empty()) {
            self.general.git_bin = value;
        }
        if let Some(value) = lookup("MEWGIT_GH_BIN").filter(|v| !v.is_empty()) {
            self.general.gh_bin = value;
        }
    }

    /// `[[repos]]` からアカウントを引く。先に書いたルールが勝つ。
    pub fn account_for(&self, host: &str, owner: &str, repo: &str) -> Option<&str> {
        let target = format!("{host}/{owner}/{repo}");
        self.repos
            .iter()
            .find(|rule| rule.matcher.is_match(&target))
            .map(|rule| rule.user.as_str())
    }
}

impl ConfigFile {
    /// glob をコンパイルして [`Config`] にする。不正なルールは捨てて警告に載せる。
    fn into_config(mut self) -> (Config, Vec<ConfigWarning>) {
        let mut warnings = Vec::new();

        // 0 件ずつ読むと 1 行も進まないまま「まだ続きがある」ページが返り、
        // 追加読み込みが止まらない（01 の「追加ページの読み込み」）。
        if self.graph.page_size == 0 {
            let fallback = Graph::default().page_size;
            warnings.push(ConfigWarning::OutOfRange {
                key: "graph.page_size",
                value: self.graph.page_size.to_string(),
                fallback: fallback.to_string(),
            });
            self.graph.page_size = fallback;
        }

        let repos = self
            .repos
            .into_iter()
            .filter_map(|spec| match compile_pattern(&spec.pattern) {
                Ok(matcher) => Some(RepoRule {
                    pattern: spec.pattern,
                    user: spec.user,
                    matcher,
                }),
                Err(err) => {
                    warnings.push(ConfigWarning::InvalidGlob {
                        pattern: spec.pattern,
                        message: err.to_string(),
                    });
                    None
                }
            })
            .collect();

        let config = Config {
            general: self.general,
            graph: self.graph,
            github: self.github,
            repos,
            keymap: self.keymap,
            theme: self.theme,
        };
        (config, warnings)
    }
}

/// `[[repos]]` の `match` をコンパイルする。
///
/// - `*` が `/` を跨がないようにしている。跨ぐと `github.com/*` が
///   `github.com/owner/repo` にも当たってしまう
/// - 大文字小文字を区別しない。GitHub の host / owner / repo は case-insensitive だが、
///   リモート URL はユーザーが打った綴りをそのまま保持するため、区別すると
///   `git@github.com:ACME/cli.git` が `github.com/acme/*` に当たらなくなる
fn compile_pattern(pattern: &str) -> Result<GlobMatcher, globset::Error> {
    Ok(GlobBuilder::new(pattern)
        .literal_separator(true)
        .case_insensitive(true)
        .build()?
        .compile_matcher())
}

/// 設定ファイルのパスを組み立てる。
///
/// `XDG_CONFIG_HOME` が絶対パスならそれを使う。XDG 仕様どおり、相対パスや未設定は
/// 無視して `$HOME/.config` にフォールバックする。どちらも決まらなければ `None`。
///
/// `home` にも絶対パスを要求する。相対パスを許すとプロセスの cwd 基準で解決され、
/// 開いているリポジトリに置かれた `.config/mewgit/config.toml` を読んでしまう。
/// 設定は `git_bin` の実行パスを持つので、これは任意コード実行になる。
pub fn resolve_config_path(xdg_config_home: Option<&Path>, home: Option<&Path>) -> Option<PathBuf> {
    let base = match xdg_config_home {
        Some(dir) if dir.is_absolute() => dir.to_path_buf(),
        _ => home.filter(|home| home.is_absolute())?.join(".config"),
    };
    Some(base.join("mewgit").join("config.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_gives_documented_defaults() {
        let (config, warnings) = Config::from_toml_str("");
        assert!(warnings.is_empty());

        assert_eq!(config.general.git_bin, "git");
        assert_eq!(config.general.gh_bin, "gh");
        assert!(!config.general.editor_fallback);
        assert_eq!(config.general.refresh_interval, 10);

        assert!(!config.graph.ascii);
        assert_eq!(config.graph.page_size, 300);
        assert_eq!(
            config.graph.colors,
            ["blue", "magenta", "cyan", "green", "yellow", "red"]
        );

        assert!(config.github.enabled);
        assert_eq!(config.github.concurrency, 4);

        assert!(config.repos.is_empty());
        assert!(config.keymap.is_empty());

        assert_eq!(config.theme.graph_head, "bold");
        assert_eq!(config.theme.pr_open, "green");
        assert_eq!(config.theme.pr_merged, "magenta");
        assert_eq!(config.theme.pr_closed, "red");
    }

    #[test]
    fn full_config_is_parsed() {
        let (config, warnings) = Config::from_toml_str(
            r#"
            [general]
            git_bin = "/usr/bin/git"
            gh_bin = "/opt/gh"
            editor_fallback = true
            refresh_interval = 0

            [graph]
            ascii = true
            page_size = 50
            colors = ["red", "blue"]

            [github]
            enabled = false
            concurrency = 1

            [[repos]]
            match = "github.com/acme-*/*"
            user = "taro_acme"

            [keymap]
            fetch = "f"
            push = "P"

            [theme]
            graph_head = "underline"
            pr_open = "cyan"
            pr_merged = "blue"
            pr_closed = "gray"
            "#,
        );

        assert!(warnings.is_empty());
        assert_eq!(config.general.git_bin, "/usr/bin/git");
        assert_eq!(config.general.gh_bin, "/opt/gh");
        assert!(config.general.editor_fallback);
        assert_eq!(config.general.refresh_interval, 0);
        assert!(config.graph.ascii);
        assert_eq!(config.graph.page_size, 50);
        assert_eq!(config.graph.colors, ["red", "blue"]);
        assert!(!config.github.enabled);
        assert_eq!(config.github.concurrency, 1);
        assert_eq!(config.keymap.get("fetch").map(String::as_str), Some("f"));
        assert_eq!(config.keymap.get("push").map(String::as_str), Some("P"));
        assert_eq!(config.theme.graph_head, "underline");
        assert_eq!(config.theme.pr_closed, "gray");
        // マッチの挙動は account_for のテストが見る。ここは読めていることだけ。
        assert_eq!(config.repos.len(), 1);
        assert_eq!(config.repos[0].pattern, "github.com/acme-*/*");
        assert_eq!(config.repos[0].user, "taro_acme");
    }

    #[test]
    fn unknown_keys_are_ignored() {
        let (config, warnings) = Config::from_toml_str(
            r#"
            [general]
            git_bin = "/usr/bin/git"
            future_option = 1

            [unknown_section]
            whatever = true
            "#,
        );
        assert!(warnings.is_empty());
        assert_eq!(config.general.git_bin, "/usr/bin/git");
    }

    #[test]
    fn broken_toml_falls_back_to_defaults_with_one_warning() {
        // 構文エラーと型不一致。どちらもファイル全体を捨てる。
        for text in ["[graph\nascii = true", "[graph]\npage_size = \"many\""] {
            let (config, warnings) = Config::from_toml_str(text);
            assert_eq!(warnings.len(), 1, "input: {text:?}");
            assert!(
                matches!(warnings[0], ConfigWarning::Parse { path: None, .. }),
                "input: {text:?}, got: {:?}",
                warnings[0]
            );
            assert_eq!(config.graph, Graph::default(), "input: {text:?}");
        }
    }

    #[test]
    fn missing_file_gives_defaults_and_no_warning() {
        let dir = tempfile::tempdir().unwrap();
        let (config, warnings) = Config::load_from(&dir.path().join("does-not-exist.toml"));
        assert!(warnings.is_empty());
        assert_eq!(config.graph, Graph::default());
    }

    #[test]
    fn load_from_reads_the_file() {
        let (_dir, path) = crate::test_util::temp_file("config.toml", "[graph]\npage_size = 42\n");
        let (config, warnings) = Config::load_from(&path);
        assert!(warnings.is_empty());
        assert_eq!(config.graph.page_size, 42);
    }

    #[test]
    fn parse_warning_carries_the_file_path() {
        let (_dir, path) = crate::test_util::temp_file("config.toml", "[graph\n");
        let (_, warnings) = Config::load_from(&path);

        assert_eq!(warnings.len(), 1);
        match &warnings[0] {
            ConfigWarning::Parse { path: slot, .. } => assert_eq!(slot.as_deref(), Some(&*path)),
            other => panic!("expected Parse, got {other:?}"),
        }
        // パスは表示にも出る。どのファイルを直せばいいか分かる必要がある。
        assert!(warnings[0]
            .to_string()
            .contains(&path.display().to_string()));
    }

    #[test]
    fn load_from_reports_io_error_for_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        let (config, warnings) = Config::load_from(dir.path());
        assert_eq!(warnings.len(), 1);
        assert!(matches!(warnings[0], ConfigWarning::Io { .. }));
        assert_eq!(config.graph, Graph::default());
    }

    #[test]
    fn account_for_matches_owner_glob() {
        let (config, warnings) = Config::from_toml_str(
            r#"
            [[repos]]
            match = "github.com/acme-*/*"
            user = "taro_acme"

            [[repos]]
            match = "github.com/codingfeline/*"
            user = "codingfeline"
            "#,
        );
        assert!(warnings.is_empty());
        assert_eq!(
            config.account_for("github.com", "acme-tools", "cli"),
            Some("taro_acme")
        );
        assert_eq!(
            config.account_for("github.com", "codingfeline", "mewgit"),
            Some("codingfeline")
        );
        assert_eq!(config.account_for("github.com", "other", "repo"), None);
        // ホストが違えば当たらない。
        assert_eq!(
            config.account_for("ghe.example.com", "acme-tools", "cli"),
            None
        );
    }

    #[test]
    fn account_for_returns_the_first_matching_rule() {
        let (config, _) = Config::from_toml_str(
            r#"
            [[repos]]
            match = "github.com/acme/*"
            user = "first"

            [[repos]]
            match = "github.com/*/*"
            user = "second"
            "#,
        );
        assert_eq!(
            config.account_for("github.com", "acme", "cli"),
            Some("first")
        );
        assert_eq!(
            config.account_for("github.com", "other", "cli"),
            Some("second")
        );
    }

    #[test]
    fn account_for_ignores_case() {
        let (config, _) = Config::from_toml_str(
            r#"
            [[repos]]
            match = "github.com/acme/*"
            user = "taro_acme"
            "#,
        );
        // リモート URL の綴りはユーザー次第だが、GitHub 側は case-insensitive。
        assert_eq!(
            config.account_for("GitHub.com", "ACME", "CLI"),
            Some("taro_acme")
        );
    }

    #[test]
    fn star_does_not_cross_slash() {
        let (config, _) = Config::from_toml_str(
            r#"
            [[repos]]
            match = "github.com/*"
            user = "too_broad"
            "#,
        );
        // `*` は `/` を跨がないので host/owner/repo の 3 要素には当たらない。
        assert_eq!(config.account_for("github.com", "acme", "cli"), None);
    }

    #[test]
    fn invalid_glob_is_skipped_with_a_warning() {
        let (config, warnings) = Config::from_toml_str(
            r#"
            [[repos]]
            match = "github.com/[bad/*"
            user = "broken"

            [[repos]]
            match = "github.com/acme/*"
            user = "ok"
            "#,
        );
        assert_eq!(warnings.len(), 1);
        match &warnings[0] {
            ConfigWarning::InvalidGlob { pattern, .. } => {
                assert_eq!(pattern, "github.com/[bad/*");
            }
            other => panic!("expected InvalidGlob, got {other:?}"),
        }
        // 壊れたルールだけ捨てて、後続のルールは生きている。
        assert_eq!(config.repos.len(), 1);
        assert_eq!(config.account_for("github.com", "acme", "cli"), Some("ok"));
    }

    #[test]
    fn config_path_resolution() {
        let home = Some("/home/user");
        let default = Some("/home/user/.config/mewgit/config.toml");
        let cases: [(Option<&str>, Option<&str>, Option<&str>); 9] = [
            (Some("/xdg"), home, Some("/xdg/mewgit/config.toml")),
            (None, home, default),
            // XDG 仕様では相対パス・空文字の XDG_CONFIG_HOME は無視する。
            (Some("relative/dir"), home, default),
            (Some(""), home, default),
            // ホームが無くても XDG が絶対パスなら決まる。
            (Some("/xdg"), None, Some("/xdg/mewgit/config.toml")),
            (None, None, None),
            (Some("relative"), None, None),
            // 相対パスの HOME も無視する。cwd 基準で解決されてしまうため。
            (None, Some("relative"), None),
            (Some("relative"), Some("relative"), None),
        ];

        for (xdg, home, want) in cases {
            let got = resolve_config_path(xdg.map(Path::new), home.map(Path::new));
            assert_eq!(
                got.as_deref(),
                want.map(Path::new),
                "xdg={xdg:?} home={home:?}"
            );
        }
    }

    #[test]
    fn env_overrides_the_file() {
        let (mut config, _) = Config::from_toml_str(
            r#"
            [general]
            git_bin = "/from/file"
            gh_bin = "/from/file"
            "#,
        );
        config.apply_env(|key| match key {
            "MEWGIT_GIT_BIN" => Some("/from/env".to_string()),
            "MEWGIT_GH_BIN" => Some("/from/env".to_string()),
            _ => None,
        });
        assert_eq!(config.general.git_bin, "/from/env");
        assert_eq!(config.general.gh_bin, "/from/env");
    }

    #[test]
    fn empty_env_var_does_not_override() {
        let (mut config, _) = Config::from_toml_str("[general]\ngit_bin = \"/from/file\"\n");
        config.apply_env(|_| Some(String::new()));
        assert_eq!(config.general.git_bin, "/from/file");
        assert_eq!(config.general.gh_bin, "gh");
    }
    #[test]
    fn a_page_size_of_zero_falls_back_to_the_default() {
        // 0 件ずつ読むと 1 行も進まないページが返り続け、追加読み込みが止まらない。
        let (config, warnings) = Config::from_toml_str("[graph]\npage_size = 0\n");

        assert_eq!(config.graph.page_size, Graph::default().page_size);
        assert_eq!(
            warnings,
            vec![ConfigWarning::OutOfRange {
                key: "graph.page_size",
                value: "0".to_string(),
                fallback: Graph::default().page_size.to_string(),
            }]
        );
    }
}
