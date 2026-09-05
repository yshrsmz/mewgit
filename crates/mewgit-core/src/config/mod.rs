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
    #[error("設定ファイルの構文エラー: {message}")]
    Parse {
        /// 文字列から直接読んだ場合は `None`。
        path: Option<PathBuf>,
        message: String,
    },
    /// `[[repos]]` の `match` が glob として不正。そのルールだけ捨てる。
    #[error("[[repos]] の match が不正な glob です: {pattern}: {message}")]
    InvalidGlob { pattern: String, message: String },
    /// ホームディレクトリが特定できず、設定ファイルの場所を決められなかった。
    #[error("ホームディレクトリを特定できず、設定ファイルを読めませんでした")]
    NoConfigDir,
}

/// `mewgit` の設定。起動時に確定し、実行中は再読込しない。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub general: General,
    pub graph: Graph,
    pub github: Github,
    pub repos: Vec<RepoRule>,
    /// アクション名 → キー表記。`KeyEvent` への変換は `mewgit-tui` 側で行う。
    pub keymap: HashMap<String, String>,
    pub theme: Theme,
}

/// `[general]`
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct General {
    pub git_bin: String,
    pub gh_bin: String,
    pub editor_fallback: bool,
}

/// `[graph]`
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Graph {
    pub ascii: bool,
    pub page_size: usize,
    pub colors: Vec<String>,
}

/// `[github]`
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Github {
    pub enabled: bool,
    pub concurrency: usize,
}

/// `[theme]`
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Theme {
    pub graph_head: String,
    pub pr_open: String,
    pub pr_merged: String,
    pub pr_closed: String,
}

/// `[[repos]]` の 1 エントリ。リポジトリごとの GitHub アカウント指定。
#[derive(Debug, Clone, Deserialize)]
pub struct RepoRule {
    /// `"{host}/{owner}/{repo}"` にマッチさせる glob。
    #[serde(rename = "match")]
    pub pattern: String,
    /// 使用する GitHub のログイン名。
    pub user: String,
    /// 読み込み時に一度だけコンパイルする。不正な glob のルールは破棄されるので
    /// 生き残ったルールでは必ず `Some`。
    #[serde(skip)]
    matcher: Option<GlobMatcher>,
}

impl Default for General {
    fn default() -> Self {
        Self {
            git_bin: "git".to_string(),
            gh_bin: "gh".to_string(),
            editor_fallback: false,
        }
    }
}

impl Default for Graph {
    fn default() -> Self {
        Self {
            ascii: false,
            page_size: 300,
            colors: ["blue", "magenta", "cyan", "green", "yellow", "red"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
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
        let xdg = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from);
        let home = directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf());

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
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return (Config::default(), Vec::new())
            }
            Err(err) => {
                return (
                    Config::default(),
                    vec![ConfigWarning::Io {
                        path: path.to_path_buf(),
                        message: err.to_string(),
                    }],
                )
            }
        };

        let (config, mut warnings) = Self::from_toml_str(&text);
        for warning in &mut warnings {
            if let ConfigWarning::Parse { path: slot, .. } = warning {
                *slot = Some(path.to_path_buf());
            }
        }
        (config, warnings)
    }

    /// TOML 文字列から読む。パースに失敗したら既定値と [`ConfigWarning::Parse`]。
    pub fn from_toml_str(text: &str) -> (Config, Vec<ConfigWarning>) {
        match toml::from_str::<Config>(text) {
            Ok(mut config) => {
                let warnings = config.compile_globs();
                (config, warnings)
            }
            Err(err) => (
                Config::default(),
                vec![ConfigWarning::Parse {
                    path: None,
                    message: err.message().to_string(),
                }],
            ),
        }
    }

    /// `[[repos]]` の glob をコンパイルする。不正なルールは破棄して警告を返す。
    fn compile_globs(&mut self) -> Vec<ConfigWarning> {
        let mut warnings = Vec::new();
        let rules = std::mem::take(&mut self.repos);
        for mut rule in rules {
            // `*` が `/` を跨がないようにする。`acme-*/*` が `a/b/c` に当たらないため。
            match GlobBuilder::new(&rule.pattern)
                .literal_separator(true)
                .build()
            {
                Ok(glob) => {
                    rule.matcher = Some(glob.compile_matcher());
                    self.repos.push(rule);
                }
                Err(err) => warnings.push(ConfigWarning::InvalidGlob {
                    pattern: rule.pattern,
                    message: err.to_string(),
                }),
            }
        }
        warnings
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
            .find(|rule| {
                rule.matcher
                    .as_ref()
                    .is_some_and(|matcher| matcher.is_match(&target))
            })
            .map(|rule| rule.user.as_str())
    }
}

/// 設定ファイルのパスを組み立てる。
///
/// `XDG_CONFIG_HOME` が絶対パスならそれを使う。XDG 仕様どおり、相対パスや未設定は
/// 無視して `$HOME/.config` にフォールバックする。どちらも決まらなければ `None`。
pub fn resolve_config_path(xdg_config_home: Option<&Path>, home: Option<&Path>) -> Option<PathBuf> {
    let base = match xdg_config_home {
        Some(dir) if dir.is_absolute() => dir.to_path_buf(),
        _ => home?.join(".config"),
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
    fn default_impl_matches_empty_toml() {
        let (parsed, _) = Config::from_toml_str("");
        let default = Config::default();
        assert_eq!(parsed.general.git_bin, default.general.git_bin);
        assert_eq!(parsed.graph.page_size, default.graph.page_size);
        assert_eq!(parsed.graph.colors, default.graph.colors);
        assert_eq!(parsed.github.enabled, default.github.enabled);
        assert_eq!(parsed.theme.pr_merged, default.theme.pr_merged);
    }

    #[test]
    fn partial_override_keeps_other_defaults() {
        let (config, warnings) = Config::from_toml_str(
            r#"
            [graph]
            ascii = true
            "#,
        );
        assert!(warnings.is_empty());
        assert!(config.graph.ascii);
        // 同じテーブル内の書かれなかった項目も既定値のまま残る。
        assert_eq!(config.graph.page_size, 300);
        assert_eq!(config.graph.colors.len(), 6);
        // 触れていないテーブルも既定値。
        assert_eq!(config.general.git_bin, "git");
        assert!(config.github.enabled);
    }

    #[test]
    fn full_config_is_parsed() {
        let (config, warnings) = Config::from_toml_str(
            r#"
            [general]
            git_bin = "/usr/bin/git"
            gh_bin = "/opt/gh"
            editor_fallback = true

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
        assert!(config.graph.ascii);
        assert_eq!(config.graph.page_size, 50);
        assert_eq!(config.graph.colors, ["red", "blue"]);
        assert!(!config.github.enabled);
        assert_eq!(config.github.concurrency, 1);
        assert_eq!(config.keymap.get("fetch").map(String::as_str), Some("f"));
        assert_eq!(config.keymap.get("push").map(String::as_str), Some("P"));
        assert_eq!(config.theme.graph_head, "underline");
        assert_eq!(config.theme.pr_closed, "gray");
        assert_eq!(
            config.account_for("github.com", "acme-tools", "cli"),
            Some("taro_acme")
        );
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
    fn parse_error_falls_back_to_defaults_with_one_warning() {
        let (config, warnings) = Config::from_toml_str("[graph\nascii = true");
        assert_eq!(warnings.len(), 1);
        assert!(matches!(
            warnings[0],
            ConfigWarning::Parse { path: None, .. }
        ));
        assert_eq!(config.graph.page_size, 300);
        assert!(!config.graph.ascii);
    }

    #[test]
    fn wrong_type_is_a_parse_warning() {
        let (config, warnings) = Config::from_toml_str(
            r#"
            [graph]
            page_size = "many"
            "#,
        );
        assert_eq!(warnings.len(), 1);
        assert!(matches!(warnings[0], ConfigWarning::Parse { .. }));
        assert_eq!(config.graph.page_size, 300);
    }

    #[test]
    fn missing_file_gives_defaults_and_no_warning() {
        let dir = tempfile::tempdir().unwrap();
        let (config, warnings) = Config::load_from(&dir.path().join("does-not-exist.toml"));
        assert!(warnings.is_empty());
        assert_eq!(config.graph.page_size, 300);
    }

    #[test]
    fn load_from_reads_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[graph]\npage_size = 42\n").unwrap();

        let (config, warnings) = Config::load_from(&path);
        assert!(warnings.is_empty());
        assert_eq!(config.graph.page_size, 42);
    }

    #[test]
    fn load_from_attaches_the_path_to_parse_warnings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[graph\n").unwrap();

        let (_, warnings) = Config::load_from(&path);
        assert_eq!(warnings.len(), 1);
        match &warnings[0] {
            ConfigWarning::Parse { path: slot, .. } => assert_eq!(slot.as_deref(), Some(&*path)),
            other => panic!("expected Parse, got {other:?}"),
        }
    }

    #[test]
    fn load_from_reports_io_error_for_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        let (config, warnings) = Config::load_from(dir.path());
        assert_eq!(warnings.len(), 1);
        assert!(matches!(warnings[0], ConfigWarning::Io { .. }));
        assert_eq!(config.graph.page_size, 300);
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
    fn config_path_prefers_absolute_xdg_config_home() {
        let path = resolve_config_path(Some(Path::new("/xdg")), Some(Path::new("/home/user")));
        assert_eq!(path.as_deref(), Some(Path::new("/xdg/mewgit/config.toml")));
    }

    #[test]
    fn config_path_falls_back_to_home_dot_config() {
        let path = resolve_config_path(None, Some(Path::new("/home/user")));
        assert_eq!(
            path.as_deref(),
            Some(Path::new("/home/user/.config/mewgit/config.toml"))
        );
    }

    #[test]
    fn config_path_ignores_relative_xdg_config_home() {
        // XDG 仕様では相対パスの XDG_CONFIG_HOME は無視する。
        let path = resolve_config_path(
            Some(Path::new("relative/dir")),
            Some(Path::new("/home/user")),
        );
        assert_eq!(
            path.as_deref(),
            Some(Path::new("/home/user/.config/mewgit/config.toml"))
        );
    }

    #[test]
    fn config_path_works_without_home_when_xdg_is_absolute() {
        let path = resolve_config_path(Some(Path::new("/xdg")), None);
        assert_eq!(path.as_deref(), Some(Path::new("/xdg/mewgit/config.toml")));
    }

    #[test]
    fn config_path_is_none_without_home_and_without_absolute_xdg() {
        assert_eq!(resolve_config_path(None, None), None);
        assert_eq!(resolve_config_path(Some(Path::new("relative")), None), None);
    }

    #[test]
    fn env_overrides_the_binaries() {
        let mut config = Config::default();
        config.apply_env(|key| match key {
            "MEWGIT_GIT_BIN" => Some("/opt/git".to_string()),
            "MEWGIT_GH_BIN" => Some("/opt/gh".to_string()),
            _ => None,
        });
        assert_eq!(config.general.git_bin, "/opt/git");
        assert_eq!(config.general.gh_bin, "/opt/gh");
    }

    #[test]
    fn env_override_beats_the_file() {
        let (mut config, _) = Config::from_toml_str("[general]\ngit_bin = \"/from/file\"\n");
        config.apply_env(|key| (key == "MEWGIT_GIT_BIN").then(|| "/from/env".to_string()));
        assert_eq!(config.general.git_bin, "/from/env");
    }

    #[test]
    fn empty_env_var_does_not_override() {
        let (mut config, _) = Config::from_toml_str("[general]\ngit_bin = \"/from/file\"\n");
        config.apply_env(|_| Some(String::new()));
        assert_eq!(config.general.git_bin, "/from/file");
        assert_eq!(config.general.gh_bin, "gh");
    }
}
