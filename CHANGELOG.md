# Changelog

## [0.1.3](https://github.com/yshrsmz/mewgit/compare/v0.1.1...v0.1.3) (2026-10-23)


### Features

* **keys:** config.keymap の上書きを表に当て、案内と dispatch の両方に効かせる ([#205](https://github.com/yshrsmz/mewgit/issues/205)) ([9879085](https://github.com/yshrsmz/mewgit/commit/98790859eb18077088a1528132c530009b4091ef))
* **keys:** 通知や画面の文言に書いたキーの字を config.keymap の上書きに追従させる ([#210](https://github.com/yshrsmz/mewgit/issues/210)) ([d1abfff](https://github.com/yshrsmz/mewgit/commit/d1abfff21dbc1584a2629fbbbcc2902a1cd44a0c))
* **remote:** 巻き戻したブランチの force push を通し、force を追跡 ref の先端を取り込んだ形に絞る ([#201](https://github.com/yshrsmz/mewgit/issues/201)) ([a220e33](https://github.com/yshrsmz/mewgit/commit/a220e337362de3bfb83e5959ccc8e58b9c1134df))
* **ui:** graph.ascii で枠線・省略の印・スピナーも ASCII に倒す ([#199](https://github.com/yshrsmz/mewgit/issues/199)) ([9937401](https://github.com/yshrsmz/mewgit/commit/9937401a62b0344b4c1a83a9bc8ce3295298803d))


### Bug Fixes

* **app:** test-util なしのビルドで seated_index が未使用になるのを直す ([#217](https://github.com/yshrsmz/mewgit/issues/217)) ([188d071](https://github.com/yshrsmz/mewgit/commit/188d0715e926e77281e8eb0b70e5305df5dfe52c))
* **exec:** 分かれたブランチの push が断られたときの案内から p を外す ([#203](https://github.com/yshrsmz/mewgit/issues/203)) ([fd73d0a](https://github.com/yshrsmz/mewgit/commit/fd73d0a9af85269429615766da0ccdfbd49d2dcc))
* **git_read:** reflog の読み取りが無いファイルを作らないようにする ([#202](https://github.com/yshrsmz/mewgit/issues/202)) ([fa5d6d6](https://github.com/yshrsmz/mewgit/commit/fa5d6d6a5578f21fbf16a31c9efa96031ffc6b9c))

## [0.1.1](https://github.com/yshrsmz/mewgit/compare/v0.1.0...v0.1.1) (2026-10-16)


### Features

* **account:** 選び直しの画面でも r で解決し直せるようにする ([#168](https://github.com/yshrsmz/mewgit/issues/168)) ([57c6e81](https://github.com/yshrsmz/mewgit/commit/57c6e810072de09cbff5f11d6f4cf26c3eca2f26))
* **app:** 1〜5 で画面の場所のペインへ直接移る ([#169](https://github.com/yshrsmz/mewgit/issues/169)) ([3943873](https://github.com/yshrsmz/mewgit/commit/3943873362782fb66c039cc8f1e88580bd69002e))
* **cli:** --version と --help を足す ([#156](https://github.com/yshrsmz/mewgit/issues/156)) ([7214ac5](https://github.com/yshrsmz/mewgit/commit/7214ac5bd1b637491707f7b6dd45490d43b1edaa))
* **config:** ロケールから graph.ascii の既定を決める ([#179](https://github.com/yshrsmz/mewgit/issues/179)) ([3c012c8](https://github.com/yshrsmz/mewgit/commit/3c012c842e7ad4ad0b04133bad03481df5ca0339))
* **release:** Release を draft で作り、公開のあとに homebrew-tap へ Formula の更新を知らせる ([#197](https://github.com/yshrsmz/mewgit/issues/197)) ([4b27139](https://github.com/yshrsmz/mewgit/commit/4b27139b59d9995c9be53caedc7867d2ef29cb7d))
* **ui:** ヘッダを決めた順に畳み、右端にスピナーの席を取る（端末幅 &lt; 100 のレイアウト 5/5） ([#166](https://github.com/yshrsmz/mewgit/issues/166)) ([a83253a](https://github.com/yshrsmz/mewgit/commit/a83253a03ecb338dcb0b51ede6b5919033adda22))
* **ui:** モーダルのキーの案内をキーの区切りで折る（端末幅 &lt; 100 のレイアウト 4/5） ([#164](https://github.com/yshrsmz/mewgit/issues/164)) ([c68140a](https://github.com/yshrsmz/mewgit/commit/c68140afe6fc1e68eb3c97ee08c2860c54470549))
* **ui:** 幅 100 未満では focus のペインだけを全画面で描く ([#160](https://github.com/yshrsmz/mewgit/issues/160)) ([7b4437c](https://github.com/yshrsmz/mewgit/commit/7b4437c1ca1c87b48719458e414a55dad19fa464))
* **ui:** 狭い右列で決めた畳み方を数え直し、折り返しの余白を ui::wrapped に置く ([#162](https://github.com/yshrsmz/mewgit/issues/162)) ([5167b78](https://github.com/yshrsmz/mewgit/commit/5167b785eaf9e7a99078cff8f8a36c9119535d67))
* **ui:** 窓に入る行だけを組み、長いパスを左で切り、PR の集計の上限を幅から導く ([#163](https://github.com/yshrsmz/mewgit/issues/163)) ([e02c3ca](https://github.com/yshrsmz/mewgit/commit/e02c3ca03f1865c9880b17330be70be53204df9f))


### Bug Fixes

* **app:** is_abort を GitOp の網羅 match にする ([#175](https://github.com/yshrsmz/mewgit/issues/175)) ([a8181ab](https://github.com/yshrsmz/mewgit/commit/a8181ab6b09a2b70bca08835d8ac8c8cfe7a6f43))
* **cli:** オプションを位置によらず拾い、余った引数を断る ([#171](https://github.com/yshrsmz/mewgit/issues/171)) ([bba8eee](https://github.com/yshrsmz/mewgit/commit/bba8eee2f0579fea101b0aadb36c73607b9e961e))
* **prs:** 既定リモートから GitHub を特定できないとき「読み込み中…」に留まらない ([#167](https://github.com/yshrsmz/mewgit/issues/167)) ([2dc77d4](https://github.com/yshrsmz/mewgit/commit/2dc77d4aa5d2800844aa696e8e23f71e0b938210))
* **scripts:** screen.py が全角の 2 桁目の印を消し残して 1 桁詰めるのを直す ([#196](https://github.com/yshrsmz/mewgit/issues/196)) ([9f4b6ea](https://github.com/yshrsmz/mewgit/commit/9f4b6ea43513b3eb2e9464376ab8cda573bd3b5a))
* **tui:** no_material の名前を GithubJob の網羅 match から引く ([#176](https://github.com/yshrsmz/mewgit/issues/176)) ([a9d7b4a](https://github.com/yshrsmz/mewgit/commit/a9d7b4a0e0cec497cad0c61b01d09e6c350b54a7))

## 0.1.0 (2026-10-04)


### Features

* **app:** A で保存したアカウントを選び直し、u で指定を解除する ([#78](https://github.com/yshrsmz/mewgit/issues/78)) ([3761360](https://github.com/yshrsmz/mewgit/commit/37613602908ce0229232cc63b012961db1307468))
* **app:** AccountPicker の r で degraded を解決し直す ([#80](https://github.com/yshrsmz/mewgit/issues/80)) ([9b03a21](https://github.com/yshrsmz/mewgit/commit/9b03a21c2312100c30187fe8d70829f4d0ea6843))
* **app:** AppState / Msg / Cmd の型定義と骨組みだけの update ([#7](https://github.com/yshrsmz/mewgit/issues/7)) ([08a0a88](https://github.com/yshrsmz/mewgit/commit/08a0a880c3295fb4542d8d7ba3e7908a272f3e32))
* **app:** fetch / pull / push を共通キー（f / p / P）に配線 ([#47](https://github.com/yshrsmz/mewgit/issues/47)) ([4176456](https://github.com/yshrsmz/mewgit/commit/417645676b28fccc6549fba6ba5e1ce5b9e297aa))
* **app:** git の失敗に「次に何を押すか」を添える ([#53](https://github.com/yshrsmz/mewgit/issues/53)) ([b01ef7b](https://github.com/yshrsmz/mewgit/commit/b01ef7b48a4f588f4587d84dd6ae3678dcb08dc8))
* **app:** Graph の v でカーソル行のコミットを revert する ([#126](https://github.com/yshrsmz/mewgit/issues/126)) ([ba7823f](https://github.com/yshrsmz/mewgit/commit/ba7823f297728d7a220de052762044dd3c110f82))
* **app:** Graph の y でカーソル行のコミットを cherry-pick する ([#124](https://github.com/yshrsmz/mewgit/issues/124)) ([b0b8122](https://github.com/yshrsmz/mewgit/commit/b0b81229ae0f33a22d1628245b3febf3b6eecd84))
* **app:** JobTracker に領域ごとの世代を持たせて 3 つの鮮度フラグを畳む ([#51](https://github.com/yshrsmz/mewgit/issues/51)) ([0bf7a51](https://github.com/yshrsmz/mewgit/commit/0bf7a510c75dad16cf9fb40b883b20915dea86b9))
* **app:** m で進行中操作のメニューを開き、continue / skip / abort を打てるようにする ([#121](https://github.com/yshrsmz/mewgit/issues/121)) ([7d270b1](https://github.com/yshrsmz/mewgit/commit/7d270b158f81bdbfd28a86c534fde3cbc806cebe))
* **app:** Modal::CommitMessage と commit / amend ([#34](https://github.com/yshrsmz/mewgit/issues/34)) ([5464b4c](https://github.com/yshrsmz/mewgit/commit/5464b4c651f58f48260296bfe5800fd81f015659))
* **app:** PR の集計は PRs タブが見えているあいだだけ引く ([#92](https://github.com/yshrsmz/mewgit/issues/92)) ([bc25258](https://github.com/yshrsmz/mewgit/commit/bc25258f2f01a06d34947fd538b3054512acde31))
* **app:** コミットのファイル一覧と DiffTarget::Commit の diff ([#39](https://github.com/yshrsmz/mewgit/issues/39)) ([abd5fc2](https://github.com/yshrsmz/mewgit/commit/abd5fc22e5c7ef152843becbe157df81b4064fe8))
* **app:** コミット詳細の遅延取得と Enter での展開表示 ([#17](https://github.com/yshrsmz/mewgit/issues/17)) ([84ed243](https://github.com/yshrsmz/mewgit/commit/84ed243a7cef13aec880dbf123b4e68c2b386184))
* **app:** スクロール追随でログの続きを読む ([#16](https://github.com/yshrsmz/mewgit/issues/16)) ([10faa0f](https://github.com/yshrsmz/mewgit/commit/10faa0fb5559298ef0911b91bc2f91896a52c41c))
* **app:** ハンク単位の stage / unstage / discard ([#31](https://github.com/yshrsmz/mewgit/issues/31)) ([a1f5c4b](https://github.com/yshrsmz/mewgit/commit/a1f5c4b5dae1043241756f88628710185b599dbf))
* **app:** ブランチの作成（n）とリネーム（r） ([#44](https://github.com/yshrsmz/mewgit/issues/44)) ([012507f](https://github.com/yshrsmz/mewgit/commit/012507f7050b8377a7162a19935030944ab5f7ce))
* **app:** ブランチの切替（Enter）と削除（d / D） ([#42](https://github.com/yshrsmz/mewgit/issues/42)) ([b374cbb](https://github.com/yshrsmz/mewgit/commit/b374cbb13b1d906467a910320dc312fa248c6556))
* **app:** ブランチ一覧の読み取りと Branches ペイン ([#40](https://github.com/yshrsmz/mewgit/issues/40)) ([fed6875](https://github.com/yshrsmz/mewgit/commit/fed6875224dee43385468566aa8cd182ee87ddbb))
* **app:** リーダースレッドを配線してグラフに 1 行 1 コミットを出す ([#13](https://github.com/yshrsmz/mewgit/issues/13)) ([38835ee](https://github.com/yshrsmz/mewgit/commit/38835ee89b68675373645aff087cc6a7095ced80))
* **app:** 実行中ジョブのスピナーと Ctrl-C での中断 ([#56](https://github.com/yshrsmz/mewgit/issues/56)) ([5917d05](https://github.com/yshrsmz/mewgit/commit/5917d05aa9aba5572ac8c7eed052d5f7e3a6757d))
* **app:** 断られた push を憶えて確認モーダル経由の force push にする ([#49](https://github.com/yshrsmz/mewgit/issues/49)) ([0a3c29f](https://github.com/yshrsmz/mewgit/commit/0a3c29f6441216918874a84b5d7f89494abb7c4f))
* **app:** 既定リモートが決まらないときに一覧から選ばせる ([#65](https://github.com/yshrsmz/mewgit/issues/65)) ([83954cd](https://github.com/yshrsmz/mewgit/commit/83954cd08071bbc22f696fb19bc294ff90e2664f))
* **app:** 通知の履歴を L で開くモーダルを足す ([#116](https://github.com/yshrsmz/mewgit/issues/116)) ([7da4d9e](https://github.com/yshrsmz/mewgit/commit/7da4d9ec5b75880e691b5bd19247cce9169f15cf))
* **app:** 通知を次の入力で下ろし、積まれた時刻を添える ([#112](https://github.com/yshrsmz/mewgit/issues/112)) ([01b832f](https://github.com/yshrsmz/mewgit/commit/01b832f1f32b48c2de313028fe640a09edb54a56))
* **app:** 進行中操作があるあいだ HEAD と作業ツリー全体を動かす書き込みを断る ([#120](https://github.com/yshrsmz/mewgit/issues/120)) ([1dc8d2a](https://github.com/yshrsmz/mewgit/commit/1dc8d2abd922d53eb3ae7e9d7146a2f839e67b99))
* **auth:** GhCli で gh から認証状態とトークンを取る ([#61](https://github.com/yshrsmz/mewgit/issues/61)) ([46777a5](https://github.com/yshrsmz/mewgit/commit/46777a5ecb3cb4220c39e8da043d2979fb5d3df8))
* **auth:** RemoteUrl パーサと ssh -G によるホスト名の正規化 ([#45](https://github.com/yshrsmz/mewgit/issues/45)) ([3cca777](https://github.com/yshrsmz/mewgit/commit/3cca777296386f77e24ab13700c362a7a1b35f30))
* **auth:** RepoInfo.target を埋める ([#63](https://github.com/yshrsmz/mewgit/issues/63)) ([7938207](https://github.com/yshrsmz/mewgit/commit/7938207eaee2865256dc58fcfc104634d94a4826))
* **auth:** アカウント解決の本体（明示指定 → 単一 → probe → Ambiguous） ([#68](https://github.com/yshrsmz/mewgit/issues/68)) ([e508d5d](https://github.com/yshrsmz/mewgit/commit/e508d5d6358a6dc5673a758c0967570fa027ae2e))
* **config:** 設定ファイルの読み込みと [[repos]] の glob マッチ ([#6](https://github.com/yshrsmz/mewgit/issues/6)) ([e207a5e](https://github.com/yshrsmz/mewgit/commit/e207a5e2f5da80fb0b802c78806142208dda6645))
* **git_exec:** git の最低版を 2.38.0 にし、古い git でも # 行を残す ([#151](https://github.com/yshrsmz/mewgit/issues/151)) ([3c9f8f4](https://github.com/yshrsmz/mewgit/commit/3c9f8f49a5315820aabd80539935585abde2096a))
* **git_exec:** HEAD が動いて断った interactive rebase を案内する ([#137](https://github.com/yshrsmz/mewgit/issues/137)) ([09803ac](https://github.com/yshrsmz/mewgit/commit/09803acef4be0e6f1f798bd03ee38bdd2d3aa302))
* **git_exec:** interactive rebase の plan を隠しサブコマンドで git に渡す ([#133](https://github.com/yshrsmz/mewgit/issues/133)) ([b0471a6](https://github.com/yshrsmz/mewgit/commit/b0471a6d79c2bbd90a2de0cfbf8f8c6f8c472ab1))
* **git_exec:** to_command と Runner、全 GitOp の argv テスト ([#21](https://github.com/yshrsmz/mewgit/issues/21)) ([17fd5cf](https://github.com/yshrsmz/mewgit/commit/17fd5cf5cc0a13ccd6b1d67c17727d40d8ee6886))
* **git_exec:** 外で変わったファイルに古いハンクを当てない ([#142](https://github.com/yshrsmz/mewgit/issues/142)) ([4907bbe](https://github.com/yshrsmz/mewgit/commit/4907bbe7c9b593037c3447e73c4c14c27a25738f))
* **git_read:** LogSession によるログ列挙とページング ([#10](https://github.com/yshrsmz/mewgit/issues/10)) ([211b61c](https://github.com/yshrsmz/mewgit/commit/211b61c4b02ddf848ee61a4edfd24ee8092c1770))
* **git_read:** RepoReader の open / remotes / refs / head ([#9](https://github.com/yshrsmz/mewgit/issues/9)) ([c81554c](https://github.com/yshrsmz/mewgit/commit/c81554cc7521fcad99baa056ce6d921c0ed02612))
* **git_read:** RepoReader を専用スレッドに置く ReaderHandle ([#12](https://github.com/yshrsmz/mewgit/issues/12)) ([2b0fb32](https://github.com/yshrsmz/mewgit/commit/2b0fb32e209dbf3a80f97c1f9a165d333fd3de60))
* **git_read:** ワーキングツリーの status と diff のハンク分割 ([#18](https://github.com/yshrsmz/mewgit/issues/18)) ([d1a594f](https://github.com/yshrsmz/mewgit/commit/d1a594fa569a17dc679ceb7f81cddfdd1f00b5d4))
* **git_read:** 進行中操作を検出してステータスバーにバッジで出す ([#118](https://github.com/yshrsmz/mewgit/issues/118)) ([128abf9](https://github.com/yshrsmz/mewgit/commit/128abf951ea372461b0483755b13c83567169955))
* **github:** reqwest 0.13 と GithubClient ([#59](https://github.com/yshrsmz/mewgit/issues/59)) ([2201fb5](https://github.com/yshrsmz/mewgit/commit/2201fb5b705617a8842e557390a6bb92fa281e80))
* **github:** 足りないスコープを応答のヘッダから判って gh auth refresh を案内する ([#110](https://github.com/yshrsmz/mewgit/issues/110)) ([749d028](https://github.com/yshrsmz/mewgit/commit/749d0288259e3a99620b5abe58b79bb05c18e77b))
* **graph:** Space で印を付けて複数のコミットを cherry-pick / revert する ([#139](https://github.com/yshrsmz/mewgit/issues/139)) ([98575c1](https://github.com/yshrsmz/mewgit/commit/98575c1cf540f46fbedb6731dcea1172c85a49c9))
* **graph:** レーン割り当てと罫線セルの生成 ([#14](https://github.com/yshrsmz/mewgit/issues/14)) ([1da6bde](https://github.com/yshrsmz/mewgit/commit/1da6bde2addde15b7d198b587194b27a1f8a94f3))
* **prs:** Enter で PR をローカルに取り出す ([#106](https://github.com/yshrsmz/mewgit/issues/106)) ([094f327](https://github.com/yshrsmz/mewgit/commit/094f327f7a5d2c088a17ab9d40c2c883f1fd9fd9))
* **prs:** fork のブランチから出した PR にカーソルを座らせる ([#98](https://github.com/yshrsmz/mewgit/issues/98)) ([726cf68](https://github.com/yshrsmz/mewgit/commit/726cf68e9878a69c368dca3643dafb73fec3040e))
* **prs:** N で PR を作る ([#101](https://github.com/yshrsmz/mewgit/issues/101)) ([922aae9](https://github.com/yshrsmz/mewgit/commit/922aae94cdfe3c283bb5e0cf68a898762ca191c9))
* **prs:** PR 作成の head を候補から選ばせる ([#107](https://github.com/yshrsmz/mewgit/issues/107)) ([f93e54f](https://github.com/yshrsmz/mewgit/commit/f93e54f90e29ed8ec6edca03ab0c6c701915928e))
* **prs:** r で PR の集計と一覧を引き直す ([#96](https://github.com/yshrsmz/mewgit/issues/96)) ([1d509ba](https://github.com/yshrsmz/mewgit/commit/1d509baf776b12b13c2e8d490302573ffe4f0288))
* **rebase:** Graph の i で RebaseEditor を開き interactive rebase を実行する ([#135](https://github.com/yshrsmz/mewgit/issues/135)) ([1d2e46b](https://github.com/yshrsmz/mewgit/commit/1d2e46b42ef508e5c12469355e051d53b8ff4a64))
* **rebase:** RebaseEditor の r で reword のメッセージを打つ ([#136](https://github.com/yshrsmz/mewgit/issues/136)) ([648846f](https://github.com/yshrsmz/mewgit/commit/648846f759532a01a81c1e84fd2191ab23e392d0))
* **stash:** Stash ペインで stash の一覧を出し、Enter で左に diff を開く ([#129](https://github.com/yshrsmz/mewgit/issues/129)) ([5ee643a](https://github.com/yshrsmz/mewgit/commit/5ee643a405b3cd2c7f81138bf6c2cf3b00373b5e))
* **stash:** Worktree の Z でカーソルの 1 ファイルを stash する ([#132](https://github.com/yshrsmz/mewgit/issues/132)) ([ff9b6ec](https://github.com/yshrsmz/mewgit/commit/ff9b6ec3d2d506409cc4e0927d92b524900a04f8))
* **stash:** z で push、Stash の a / Z / d で apply / pop / drop ([#131](https://github.com/yshrsmz/mewgit/issues/131)) ([9d276b8](https://github.com/yshrsmz/mewgit/commit/9d276b87f6527896a1c2d711e7177a78a0540ac0))
* **tui:** 端末の初期化・復元とイベントループを実装（M0 完了） ([#8](https://github.com/yshrsmz/mewgit/issues/8)) ([1528e6e](https://github.com/yshrsmz/mewgit/commit/1528e6e9b7d2b214a2015cee4a1d50caea433868))
* **ui:** ? でキーの一覧を開き、フッターを「ペインのキー + ?:help q:quit」にする ([#127](https://github.com/yshrsmz/mewgit/issues/127)) ([135307f](https://github.com/yshrsmz/mewgit/commit/135307f424aa45371cb4737a39e0c1210c8090d5))
* **ui:** A でアカウントの内訳を出し、選んだ login を .git/config に保存する ([#73](https://github.com/yshrsmz/mewgit/issues/73)) ([67bef46](https://github.com/yshrsmz/mewgit/commit/67bef46616fe4ea2f8ea682caee9f4a2c81edbad))
* **ui:** Diff ペイン。左の領域を Graph と差し替えて出す ([#24](https://github.com/yshrsmz/mewgit/issues/24)) ([d4d1538](https://github.com/yshrsmz/mewgit/commit/d4d153804b897f10261d215c54517ff400953de8))
* **ui:** GitHub API の残量をヘッダに出す ([#90](https://github.com/yshrsmz/mewgit/issues/90)) ([295f040](https://github.com/yshrsmz/mewgit/commit/295f04024dbeb76911ce593e866dc215c7ce9db2))
* **ui:** PR ごとにチェックとレビューの集計を引いて行に出す ([#89](https://github.com/yshrsmz/mewgit/issues/89)) ([b5b90ca](https://github.com/yshrsmz/mewgit/commit/b5b90ca53739397719f9662e355304293468a429))
* **ui:** PRs タブに現ブランチの PR とチェックを出す ([#83](https://github.com/yshrsmz/mewgit/issues/83)) ([6f87eb8](https://github.com/yshrsmz/mewgit/commit/6f87eb808d8d0865b621950861085ad316aed41a))
* **ui:** PRs ペインを open な PR の一覧にする ([#86](https://github.com/yshrsmz/mewgit/issues/86)) ([679354b](https://github.com/yshrsmz/mewgit/commit/679354b8ff473ee1b8d69b8880153644bc401479))
* **ui:** worktree ペインとファイル単位の stage / unstage / discard ([#22](https://github.com/yshrsmz/mewgit/issues/22)) ([1ad40d0](https://github.com/yshrsmz/mewgit/commit/1ad40d0076a590564dea476e9e86415d7f6276ab))
* **ui:** グラフの罫線を描く ([#15](https://github.com/yshrsmz/mewgit/issues/15)) ([8c17e50](https://github.com/yshrsmz/mewgit/commit/8c17e50e83e6833b839099d4bfff0f6d11f03c5d))
* **ui:** コミット詳細を右ペインへ移し、グラフ先頭に WIP 行を置く ([#36](https://github.com/yshrsmz/mewgit/issues/36)) ([0525bb4](https://github.com/yshrsmz/mewgit/commit/0525bb49d118fbac9902ca54cdd2a15629cc2020))
* **ui:** フォーカス中のペインを太線と色で、出ているタブを反転で示す ([#117](https://github.com/yshrsmz/mewgit/issues/117)) ([d874aec](https://github.com/yshrsmz/mewgit/commit/d874aecadd702e000a382efd36251e02fa23ac33))
* **ui:** フッターを 2 行にしてフォーカス中のペインのキーを出す ([#94](https://github.com/yshrsmz/mewgit/issues/94)) ([4a5f9d5](https://github.com/yshrsmz/mewgit/commit/4a5f9d5c398075467d0d3173c45aa5819755c381))
* **ui:** ヘッダに [gh: login @ host] を出し、アカウント解決を配線する ([#72](https://github.com/yshrsmz/mewgit/issues/72)) ([49e2ee4](https://github.com/yshrsmz/mewgit/commit/49e2ee4f4c39733dd4eef825b685377daa264129))
* **watch:** 外の変更を検知して読み直す ([#141](https://github.com/yshrsmz/mewgit/issues/141)) ([bff763d](https://github.com/yshrsmz/mewgit/commit/bff763db82980d093b6f3ce4bb14564265d234ee))
* プロジェクトの足場と開発環境を整える ([#3](https://github.com/yshrsmz/mewgit/issues/3)) ([50fa787](https://github.com/yshrsmz/mewgit/commit/50fa7879cc03bcf17a20a70188194020a1b94715))


### Bug Fixes

* **account:** mewgit.user の書き込みを出した時点で Resolving に落とす ([#149](https://github.com/yshrsmz/mewgit/issues/149)) ([8417ae1](https://github.com/yshrsmz/mewgit/commit/8417ae1d00a3054f375aa7b58c162418b49a15bb))
* **app:** 取り出した fork のブランチの名前がずれていても現ブランチの PR に座る ([#108](https://github.com/yshrsmz/mewgit/issues/108)) ([5560a65](https://github.com/yshrsmz/mewgit/commit/5560a6566b683d2df621a627e0dc0fe14bdb53c8))
* **app:** 差分を出せない行で Diff ペインに入れないようにする ([#54](https://github.com/yshrsmz/mewgit/issues/54)) ([71e6c1d](https://github.com/yshrsmz/mewgit/commit/71e6c1df43cbfdeaf3b6667745da820606bcee3a))
* **deps:** update rust crate git2 to 0.21 ([#27](https://github.com/yshrsmz/mewgit/issues/27)) ([b58165d](https://github.com/yshrsmz/mewgit/commit/b58165d7de67d468e99c6819692b618f8481af19))
* **deps:** update rust crate ratatui to 0.30 (crossterm 0.29 と同時) ([#28](https://github.com/yshrsmz/mewgit/issues/28)) ([7ed0a58](https://github.com/yshrsmz/mewgit/commit/7ed0a58b92e1132d35cac31bae31255dd46e5e30))
* **deps:** update rust crate toml to v1 ([#33](https://github.com/yshrsmz/mewgit/issues/33)) ([b69d48b](https://github.com/yshrsmz/mewgit/commit/b69d48b08b47bb400147d8311e3d89ebdb5207fb))
* **git_exec:** hook が断った reword を continue の前に amend する ([#145](https://github.com/yshrsmz/mewgit/issues/145)) ([a249513](https://github.com/yshrsmz/mewgit/commit/a24951384fa614480a325258213778588fa4174a))
* **git_exec:** stderr にだけ理由がある失敗の要約行に、その理由を出す ([#147](https://github.com/yshrsmz/mewgit/issues/147)) ([f3b6bee](https://github.com/yshrsmz/mewgit/commit/f3b6beef8f2b490223146feed634e8ee5c216ebf))
* **git_exec:** stdout にだけ出る失敗の理由を通知に出す ([#146](https://github.com/yshrsmz/mewgit/issues/146)) ([29b0a16](https://github.com/yshrsmz/mewgit/commit/29b0a16ac6d6559403f9d56aeea027b72a761683))
* **git_exec:** 起動中に消えた自バイナリのエディタを案内し、Linux では起こせるようにする ([#150](https://github.com/yshrsmz/mewgit/issues/150)) ([09baa1b](https://github.com/yshrsmz/mewgit/commit/09baa1bd2164dce834c78e2d597e3606649ac0ed))
* **release:** App の client ID を secrets から読む ([#153](https://github.com/yshrsmz/mewgit/issues/153)) ([da7d646](https://github.com/yshrsmz/mewgit/commit/da7d646569cf4019dcc3a50e7f8262caf84ac9c3))
* **remote:** push の行き先を pushRemote / pushDefault に揃える ([#148](https://github.com/yshrsmz/mewgit/issues/148)) ([7836e0c](https://github.com/yshrsmz/mewgit/commit/7836e0c642735fb75ad6af599db4282010232765))
* **ui:** 確認モーダルの折り返し行数を実測にする ([#23](https://github.com/yshrsmz/mewgit/issues/23)) ([9261cfb](https://github.com/yshrsmz/mewgit/commit/9261cfb5c0dddfcce7cc9abf0836bcf411695c9c))
