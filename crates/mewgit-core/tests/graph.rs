//! `graph::LaneState` を実リポジトリのログに掛ける統合テスト。
//!
//! 固定のコミット列に対する `cells` の期待値は `graph::lane` のユニットテストにある。
//! ここで見たいのは「`git_read` が返す順のコミット列に対して、レーンの割り当てが
//! 成り立っているか」だけ。`git log --graph` の出力と文字単位では比べない（git 側の
//! 整形規則に追従する意味が無い）。

use std::collections::HashMap;

use mewgit_core::git_read::{Commit, RepoReader};
use mewgit_core::graph::{LaneState, Vertical};
use mewgit_core::test_util::TestRepo;

/// main と feature を分岐させてマージしたリポジトリ。
fn merged_history() -> TestRepo {
    let repo = TestRepo::init();
    repo.commit_file("a.txt", "first");
    repo.commit_file("a.txt", "second");
    repo.git(&["checkout", "-b", "feature"]);
    repo.commit_file("b.txt", "feature work");
    repo.git(&["checkout", "-"]);
    repo.commit_file("a.txt", "third");
    repo.git(&["merge", "--no-ff", "-m", "merge feature", "feature"]);
    repo
}

/// ログを 1 ページで読む。
fn commits(repo: &TestRepo) -> Vec<Commit> {
    let reader = RepoReader::open(repo.path()).expect("開けませんでした");
    let mut session = reader.log().expect("ログを開けませんでした");
    let page = session.next_page(100).expect("ログを読めませんでした");
    assert!(page.done, "1 ページで読み切れていません");
    page.commits
}

#[test]
fn every_commit_gets_a_row_whose_cells_reach_its_lane() {
    let repo = merged_history();
    let commits = commits(&repo);

    let rows = LaneState::default().extend(&commits);

    assert_eq!(rows.len(), commits.len());
    for (row, commit) in rows.iter().zip(&commits) {
        assert_eq!(row.oid, commit.oid);
        assert!(
            row.cells.len() > row.lane,
            "レーン {} の行にセルが {} 個しかありません",
            row.lane,
            row.cells.len()
        );
        assert!(
            matches!(row.cells[row.lane].vertical, Vertical::Commit { merge, .. }
                if merge == (commit.parents.len() > 1)),
            "コミットのマスが本体になっていません: {:?}",
            row.cells[row.lane]
        );
    }
}

#[test]
fn a_first_parent_never_moves_to_a_lane_further_right() {
    // 06 の「`p1` を `c.lane` に置く（同じ色を継承）」。合流で最左に寄るので
    // 「同じレーンに出る」までは言えない。言えるのは「右にずれない」と
    // 「同じレーンなら色を継ぐ」の 2 つ。
    let repo = merged_history();
    let commits = commits(&repo);

    let rows = LaneState::default().extend(&commits);
    let by_oid: HashMap<_, _> = rows.iter().map(|row| (row.oid, row)).collect();

    let merges = rows
        .iter()
        .filter(|row| {
            matches!(
                row.cells[row.lane].vertical,
                Vertical::Commit { merge: true, .. }
            )
        })
        .count();
    assert_eq!(merges, 1, "マージコミットが 1 つあるはずです");

    for (row, commit) in rows.iter().zip(&commits) {
        let Some(first_parent) = commit.parents.first().and_then(|oid| by_oid.get(oid)) else {
            continue;
        };
        assert!(
            first_parent.lane <= row.lane,
            "第 1 親が右のレーンに移りました: {} → {}",
            row.lane,
            first_parent.lane
        );
        if first_parent.lane == row.lane {
            assert_eq!(
                first_parent.color, row.color,
                "同じレーンで色が変わりました"
            );
        }
    }
}
