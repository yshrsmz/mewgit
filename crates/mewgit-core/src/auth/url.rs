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
        let url = url.trim();
        parse_inner(url).ok_or_else(|| AuthError::InvalidRemoteUrl { url: redacted(url) })
    }
}

fn parse_inner(url: &str) -> Option<RemoteUrl> {
    let (authority, path, scheme) = if let Some(rest) = url
        .strip_prefix("ssh://")
        // `git+ssh://` は git が ssh 経路に流す（`core.sshCommand` で argv をダンプして
        // 確認）。大文字の `SSH://` は git 自身が `remote-SSH` ヘルパを探して落ちるので
        // 受けない。同じ理由で scheme の照合は大小文字を区別したままにする。
        .or_else(|| url.strip_prefix("git+ssh://"))
    {
        let (authority, path) = rest.split_once('/')?;
        (authority, path, RemoteScheme::Ssh)
    } else if let Some(rest) = url.strip_prefix("https://") {
        let (authority, path) = rest.split_once('/')?;
        (authority, path, RemoteScheme::Https)
    } else if url.contains("://") {
        // `git://` は認証が効かず、`http://` は平文、`file://` と `rsync://` は
        // GitHub のリモートではない。scp 形式と誤って読まないよう、ここで落とす。
        return None;
    } else {
        // scp 形式 `[user@]host:owner/repo`。`://` を含まない文字列のうち、
        // **最初の `/` より前に `:` があるもの**だけが scp 形式。
        let (authority, path) = url.split_once(':')?;
        // authority に `/` があればローカルパス（`~/repos/x@host:a/b.git` など）。
        // **`valid_host` では落ちない** — `host_of` が最後の `@` から後ろだけを見るので、
        // `/` を含む部分がそこで消える。
        if authority.contains('/') {
            return None;
        }
        (authority, path, RemoteScheme::Ssh)
    };

    // 確保はここ 1 か所。読めない URL では 1 つも作らない。
    let host = host_of(authority)?;
    let (owner, repo) = split_owner_repo(path)?;
    Some(RemoteUrl {
        scheme,
        host_raw: host.to_string(),
        owner: owner.to_string(),
        repo: repo.to_string(),
    })
}

/// `[user@]host[:port]` からホストだけを取る。
fn host_of(authority: &str) -> Option<&str> {
    // user 名に `@` が入ることがあるので、最後の `@` で割る。
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    // port を落とす。IPv6 リテラル（`[::1]:22`）は `[` を含むので下の検査で落ちる。
    let host = host.split_once(':').map_or(host, |(host, _port)| host);
    valid_host(host).then_some(host)
}

/// ホスト名として受ける文字だけでできているか。
///
/// **`-` 始まりを弾くのが眼目。** 理由は 2 つある。第一に、ホスト名の構文として
/// `-` 始まりは受けない（`https://-4/a/b` を拒むのはこちらの理由）。第二に、
/// [`host_of`] が `user@` を捨てるので、通すと**裸の** `-4` が `ssh -G` の positional
/// に載る（04 の「alias は argv の positional に載る」）。
///
/// **git と同じ判断ではない。** git 2.55 が拒むのは `user@host` 全体が `-` で始まる
/// ときだけで、`git@-4:a/b.git` も `git@-oProxyCommand=id:a/b.git` も素通りする
/// （`core.sshCommand` で argv をダンプして確認。`git@-4` を 1 語で ssh に渡すので
/// オプションにならない）。user を捨てるこちらだけが裸の host を作る。
fn valid_host(host: &str) -> bool {
    !host.is_empty()
        && !host.starts_with('-')
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

/// `[/]owner/repo[.git][/]` を割る。
fn split_owner_repo(path: &str) -> Option<(&str, &str)> {
    let path = path.trim_matches('/');
    let (owner, repo) = path.split_once('/')?;
    let repo = repo.strip_suffix(".git").unwrap_or(repo);
    // GitHub のパスは 2 段だけ。3 段以上（GitLab のサブグループなど）は owner を
    // 決められないので受けない。`owner` が空になる経路は無い（先頭の `/` は落として
    // あるので、`split_once` が `Some` を返す時点で左は非空）。
    if repo.contains('/') || !valid_segment(owner) || !valid_segment(repo) {
        return None;
    }
    Some((owner, repo))
}

/// owner / repo として受ける形か。
///
/// **`valid_host` と対称に数える。** この 2 つは `GET /repos/{owner}/{repo}` に補間される
/// （05）ので、`..` を通すと `/repos/../users` になり、`?` や `#` を通すと query や
/// fragment になる。`.git/config` の値は clone 由来で任意なので、ここで止める。
fn valid_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment != "."
        && segment != ".."
        && segment
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

/// エラーに載せる前に、URL に埋め込まれた資格情報を落とす。
///
/// `https://x-access-token:<PAT>@host/owner/repo` の形は珍しくない（CI の checkout が
/// 書く）。**解析に失敗した URL は通知にもログにも出る**ので、そのまま載せると
/// トークンが漏れる（不変ルール 3）。scp 形式にパスワードは書けないので、`://` を
/// 含む形だけを見る。
fn redacted(url: &str) -> String {
    let Some(scheme_end) = url.find("://") else {
        return url.to_string();
    };
    let authority_start = scheme_end + "://".len();
    let authority_end = url[authority_start..]
        .find('/')
        .map_or(url.len(), |offset| authority_start + offset);
    match url[authority_start..authority_end].rsplit_once('@') {
        Some((_userinfo, host)) => format!(
            "{}***@{host}{}",
            &url[..authority_start],
            &url[authority_end..]
        ),
        None => url.to_string(),
    }
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

    /// 読めない URL であることを確かめる。2 つのテストが同じ形で使う。
    fn rejected(url: &str) {
        match RemoteUrl::parse(url) {
            Err(AuthError::InvalidRemoteUrl { .. }) => {}
            other => panic!("{url} を受けてしまった: {other:?}"),
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
        // git が ssh 経路に流す形（実測）。
        assert_eq!(
            parsed("git+ssh://git@github.com/a/b.git"),
            ssh("github.com", "a", "b")
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
            // `:` はあるが、その手前に `/` があるローカルパス。**`valid_host` では
            // 落ちない** — `host_of` が最後の `@` から後ろだけを見るので、
            // `~/repos/x` の部分が消えて `host` だけが残る。
            "~/repos/x@host:a/b.git",
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
            rejected(url);
        }
    }

    #[test]
    fn rejects_owner_or_repo_that_would_change_the_api_path() {
        // この 2 つは `GET /repos/{owner}/{repo}` に補間される（05）。`..` は
        // `/repos/../users` になり、`?` と `#` は query / fragment になる。
        for url in [
            "https://github.com/../users",
            "https://github.com/a/..",
            "https://github.com/./b",
            "https://github.com/a/b?x=1",
            "https://github.com/a/b#frag",
            "https://github.com/a%2fb/c",
            "git@github.com:a/b c.git",
        ] {
            rejected(url);
        }
    }

    #[test]
    fn keeps_credentials_out_of_the_error() {
        // CI の checkout が書く形。解析に失敗した URL は通知に出るので、そのまま
        // 載せるとトークンが漏れる（不変ルール 3）。
        let err = RemoteUrl::parse("https://x-access-token:ghp_secret@github.com/a/b/c")
            .expect_err("3 段のパスは受けない");
        let message = err.to_string();
        assert!(!message.contains("ghp_secret"), "{message}");
        assert!(!message.contains("x-access-token"), "{message}");
        // どのリモートで失敗したかは残す。
        assert!(message.contains("github.com/a/b/c"), "{message}");
    }

    #[test]
    fn rejects_option_like_host() {
        // `-` 始まりは 2 つの理由で受けない。ホスト名の構文として不正であること
        // （`https://-4/a/b` はこちら）と、`host_of` が `user@` を捨てるので裸の値が
        // `ssh -G` の positional に載ること（04）。**git は `git@-4:a/b.git` を通す**
        // ので、git に合わせると後者が塞げない（`valid_host` の doc に実測がある）。
        for url in [
            "git@-oProxyCommand=id:a/b.git",
            "git@-4:a/b.git",
            "ssh://git@-4/a/b",
            "https://-4/a/b",
            "-4:a/b",
        ] {
            rejected(url);
        }
    }
}
