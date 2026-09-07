//! リモート URL の解析。設計は `docs/design/04-auth-account.md` の
//! 「SSH エイリアスの正規化」。
//!
//! `.git/config` の `remote.<name>.url` から host / owner / repo を取り出す。
//! ssh の `host_raw` はエイリアスのことがあるので、[`super::ssh::resolve_ssh_host`]
//! で正規化してから [`super::RepoTarget`] を組む。

use super::AuthError;

/// リモート URL の種別。GitHub に繋がる形だけを受ける。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteScheme {
    /// `git@host:owner/repo.git` と `ssh://git@host/owner/repo`。
    Ssh,
    /// `https://host/owner/repo`。
    Https,
}

/// 解析済みのリモート URL。
///
/// user と port は捨てる。どちらも host / owner / repo を決めるのに要らず、
/// port が要るのは非既定ポートの GHES で API のベース URL を組むときだけなので、
/// そちらは `github` のタスクへ送った（04 / 05）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteUrl {
    pub scheme: RemoteScheme,
    /// URL に書かれたままのホスト。ssh ではエイリアスのことがある。
    pub host_raw: String,
    pub owner: String,
    pub repo: String,
}

impl RemoteUrl {
    /// リモート URL を解析する。
    ///
    /// GitHub 機能に使えない形（`git://` / `http://` / `file://` / ローカルパスと、
    /// `owner/repo` の 2 段にならないパス）は [`AuthError::InvalidRemoteUrl`] で拒む。
    /// **拒んでも Git 操作は動く**（GitHub 機能が degraded になるだけ）ので、
    /// 読めない形を無理に通さない。
    pub fn parse(url: &str) -> Result<Self, AuthError> {
        parse_inner(url.trim()).ok_or_else(|| AuthError::InvalidRemoteUrl {
            url: url.to_string(),
        })
    }
}

fn parse_inner(url: &str) -> Option<RemoteUrl> {
    if let Some(rest) = url.strip_prefix("ssh://") {
        from_authority(rest, RemoteScheme::Ssh)
    } else if let Some(rest) = url.strip_prefix("https://") {
        from_authority(rest, RemoteScheme::Https)
    } else if url.contains("://") {
        // `git://` は認証が効かず、`http://` は平文、`file://` と `rsync://` は
        // GitHub のリモートではない。scp 形式と誤って読まないよう、ここで落とす。
        None
    } else {
        from_scp(url)
    }
}

/// `[user@]host[:port]/owner/repo` を割る。
fn from_authority(rest: &str, scheme: RemoteScheme) -> Option<RemoteUrl> {
    let (authority, path) = rest.split_once('/')?;
    let (owner, repo) = split_owner_repo(path)?;
    Some(RemoteUrl {
        scheme,
        host_raw: host_of(authority)?,
        owner,
        repo,
    })
}

/// scp 形式 `[user@]host:owner/repo` を割る。
///
/// `://` を含まない文字列のうち、**最初の `/` より前に `:` があるもの**だけが scp 形式。
/// ローカルパス（`/srv/repos/x.git`）と区別が付く。
fn from_scp(url: &str) -> Option<RemoteUrl> {
    let (authority, path) = url.split_once(':')?;
    if authority.contains('/') {
        return None;
    }
    let (owner, repo) = split_owner_repo(path)?;
    Some(RemoteUrl {
        scheme: RemoteScheme::Ssh,
        host_raw: host_of(authority)?,
        owner,
        repo,
    })
}

/// `[user@]host[:port]` からホストだけを取る。
fn host_of(authority: &str) -> Option<String> {
    // user 名に `@` が入ることがあるので、最後の `@` で割る。
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    // port を落とす。IPv6 リテラル（`[::1]:22`）は `[` を含むので下の検査で落ちる。
    let host = host.split_once(':').map_or(host, |(host, _port)| host);
    valid_host(host).then(|| host.to_string())
}

/// ホスト名として受ける文字だけでできているか。
///
/// **`-` 始まりを弾くのが眼目**（04 の「alias は argv の positional に載る」）。
/// ssh の host_raw は `ssh -G` の positional に載るので、`-oProxyCommand=...` のような
/// 値をここで止める。git 自身も `-` 始まりのホストを拒む。
fn valid_host(host: &str) -> bool {
    !host.is_empty()
        && !host.starts_with('-')
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

/// `[/]owner/repo[.git][/]` を割る。
fn split_owner_repo(path: &str) -> Option<(String, String)> {
    let path = path.trim_matches('/');
    let (owner, repo) = path.split_once('/')?;
    let repo = repo.strip_suffix(".git").unwrap_or(repo);
    // GitHub のパスは 2 段だけ。3 段以上（GitLab のサブグループなど）は owner を
    // 決められないので受けない。
    if owner.is_empty() || repo.is_empty() || repo.contains('/') {
        return None;
    }
    Some((owner.to_string(), repo.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(url: &str) -> RemoteUrl {
        RemoteUrl::parse(url).unwrap_or_else(|err| panic!("{url} を解析できなかった: {err}"))
    }

    fn ssh(host: &str, owner: &str, repo: &str) -> RemoteUrl {
        RemoteUrl {
            scheme: RemoteScheme::Ssh,
            host_raw: host.to_string(),
            owner: owner.to_string(),
            repo: repo.to_string(),
        }
    }

    fn https(host: &str, owner: &str, repo: &str) -> RemoteUrl {
        RemoteUrl {
            scheme: RemoteScheme::Https,
            ..ssh(host, owner, repo)
        }
    }

    #[test]
    fn parses_scp_form() {
        assert_eq!(
            parsed("git@github.com:yshrsmz/mewgit.git"),
            ssh("github.com", "yshrsmz", "mewgit")
        );
        // user 無しと、`:` の直後の `/`。どちらも git が受ける形。
        assert_eq!(
            parsed("gh-work:acme/tools"),
            ssh("gh-work", "acme", "tools")
        );
        assert_eq!(
            parsed("git@gh-work:/acme/tools.git"),
            ssh("gh-work", "acme", "tools")
        );
    }

    #[test]
    fn parses_ssh_url() {
        assert_eq!(
            parsed("ssh://git@github.com/yshrsmz/mewgit.git"),
            ssh("github.com", "yshrsmz", "mewgit")
        );
        // port は捨てる（04）。
        assert_eq!(
            parsed("ssh://git@acme.ghe.com:2222/acme/tools"),
            ssh("acme.ghe.com", "acme", "tools")
        );
    }

    #[test]
    fn parses_https_url() {
        assert_eq!(
            parsed("https://github.com/yshrsmz/mewgit.git"),
            https("github.com", "yshrsmz", "mewgit")
        );
        // 末尾の `/` と、URL に埋め込まれた user。
        assert_eq!(
            parsed("https://token@acme.ghe.com:8443/acme/tools/"),
            https("acme.ghe.com", "acme", "tools")
        );
    }

    #[test]
    fn trims_surrounding_whitespace() {
        // `.git/config` の値は改行付きで取れることがある。
        assert_eq!(
            parsed("  https://github.com/a/b.git\n"),
            https("github.com", "a", "b")
        );
    }

    #[test]
    fn rejects_unsupported_forms() {
        for url in [
            "git://github.com/a/b.git",
            "http://github.com/a/b.git",
            "file:///srv/repos/b.git",
            "rsync://github.com/a/b.git",
            // ローカルパス。`:` が無いので scp 形式にもならない。
            "/srv/repos/b.git",
            "../sibling",
            // owner か repo が足りない / 多い。
            "https://github.com/a",
            "git@github.com:a",
            "https://github.com/a/b/c",
            "https://github.com//b.git",
            "https://github.com/a/.git",
            // ホストが無い。
            "https:///a/b.git",
            "ssh://git@github.com",
            "",
        ] {
            assert!(
                matches!(
                    RemoteUrl::parse(url),
                    Err(AuthError::InvalidRemoteUrl { .. })
                ),
                "{url} を受けてしまった: {:?}",
                RemoteUrl::parse(url)
            );
        }
    }

    #[test]
    fn rejects_option_like_host() {
        // `host_raw` は `ssh -G` の positional に載る。`-` 始まりを通すと、clone して
        // きたリポジトリの `.git/config` が ssh のオプションになる（04）。
        for url in [
            "git@-oProxyCommand=id:a/b.git",
            "git@-4:a/b.git",
            "ssh://git@-4/a/b",
            "https://-4/a/b",
            "-4:a/b",
        ] {
            assert!(
                matches!(
                    RemoteUrl::parse(url),
                    Err(AuthError::InvalidRemoteUrl { .. })
                ),
                "{url} を受けてしまった: {:?}",
                RemoteUrl::parse(url)
            );
        }
    }
}
