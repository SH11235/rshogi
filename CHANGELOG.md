# Changelog

リリースごとの主要変更点と移行手順をまとめる。詳細は各 PR / runbook を参照。

GitHub Release tag は engine 全体 (USI engine + CSA server + tools + spsa) の release
marker として `vX.Y.Z` を打つ。crates.io 上の `rshogi-core` は別系列 (0.x semver) で
運用しており、library API の互換性は core のバージョンで判断する
(`crates/rshogi-core/Cargo.toml`)。

crates.io への `rshogi-core` publish は engine release とは独立して行う。前回 publish 以降の
core 変更を公開する PR では `crates/rshogi-core/Cargo.toml` のバージョンを必ず bump し、
その PR の merge commit から publish する。`vX.Y.Z` タグは engine 全体の release marker
専用であり、core 単独 publish のためのタグは打たない。

## Unreleased

### tournament / spsa: 対局履歴の送信

- 共有対局 driver は毎手の現在 SFEN の代わりに、対局開始局面と全指し手を
  `position sfen ... moves ...` で送信する。エンジンが反復・連続王手の履歴を探索で
  利用できるようにした。履歴の基点は開始局面ファイルの手順適用後。
- 履歴は毎局・再試行でリセットし、pass 権は基点の値から手順再生で消費する。
  旧 driver と探索条件が異なるため、評価実験・SPSA の結果は更新前後を混在させない。
- エンジン応答と開始局面ファイルの手順は、完全合法手集合で検査する。疑似合法手を
  前提とする検査で不正手を通し、空き升からの移動などで異常終了する経路も修正した。

### tournament: ルールによる自動終局

- 千日手を 4 回同一局面で自動終局し、連続王手の千日手は王手側の反則負けとする。
  共有 driver を使う SPSA にも適用する。
- `--adjudicate-resign` / `--adjudicate-draw` に評価値裁定を追加（既定 off）。
  千日手と引分裁定は通常の引分として WLD / pentanomial に算入する。
- `analyze_selfplay` に全 result 行の終局理由分布と `max_moves` 到達率を追加した。

### rescore_psv: routing bucket 数の防御と NNUE score sidecar

- `--ls-progress-buckets` が NNUE ファイルの格納 bucket 数と不一致の場合はエラーに
  なる（合法設定として素通りし全行を無警告で誤評価にしていた）。格納数より少ない
  routing で学習された旧世代 net の評価は新フラグ
  `--allow-routing-buckets-mismatch` で警告付きに許可する。
- `--out-scores`（i16 score sidecar）を NNUE 静的評価モードに開放した。dlshogi
  ONNX 経路と同じ `.in-progress` / `.done` marker による中断再開・fail-closed
  （エラーを含むチャンクは 1 byte も書かない）で、fingerprint は NNUE の
  path/size/mtime と routing 設定（progress 係数は sha256）・FV_SCALE 変換を追跡する。

### 破壊的変更: LayerStacks routing の明示化

- bucket mode 名 `progress8kpabs` を `progresskpabs` へ変更した。旧名の alias はなく、指定するとエラーになる。
- LayerStacks 推論では mode と構造を明示する。USI の progresskpabs は
  `LS_BUCKET_MODE=progresskpabs`、`LS_PROGRESS_BUCKETS=<学習時の bucket 数>`、
  `LS_PROGRESS_COEFF=<progress.bin>` が必須。KingRank9 は `LS_BUCKET_MODE=kingrank9` のみを指定する。
- NNUE version は binary layout の判別専用で、routing semantics の推測には使わない。
  特に旧 F20 の格納9 bucket モデルを `floor(p×8)` で学習した場合は
  `LS_PROGRESS_BUCKETS=8` を指定する。未指定、格納数超過、KingRank9 と格納数9以外の組合せは
  `isready` / native tool 初期化時にエラーになる。
- `LS_PROGRESS_BUCKETS=1` は常に bucket 0 を選ぶ no-op routing として許可する
  (格納 1 bucket net の設定経路)。この場合のみ `LS_PROGRESS_COEFF` は不要。
- 公開 API の破壊的 rename (`Progress8KPAbs` → `ProgressKPAbs` 等) に伴い
  `rshogi-core` を 0.6.0 へ bump。
- gensfen: native LayerStacks run の generation fingerprint に `bucket_mode` /
  `progress_buckets` が加わるため、本変更前に開始した native LayerStacks run は
  resume できない (fingerprint 不一致で fail)。新規 run として作り直すこと。

## v1.4.0 — 2026-08-13

v1.3.0 後の性能改善と教師データパイプライン拡張のリリース。NNUE 推論・探索・局面処理の
性能改善バッチ、dual-label PSV (1 レコード内に base + DL の 2 ラベルを原子的に保持する
教師形式) の生成・検証パイプライン、LayerStacks の KingRank9 bucket mode と
runtime-dimensional universal edition、CSA server の入玉宣言ルール設定駆動化と検索・観戦系
API、gensfen の resume / クラッシュ耐性、Windows 対応の一括修正が中心。

詳細は各 PR を参照。

### 性能改善 (NNUE / 探索 / 局面処理)

特記なき項目は search_only_ab による ABBA x3 の固定時間計測 (括弧は測定窓)。数値は各 PR 本文の実測。

- **LayerStacks L1 activation の AVX2 化** (#983): NPS +1.62% (10s)
- **Finny accumulator 更新の tile 化** (#984): NPS +3.54% (10s)。AVX-512 有効 build 向けの
  zmm tile path は #996 で追加
- **LayerStacks copy + diff 更新の融合** (#985): NPS +4.12% (10s)
- **LayerStacks 出力活性の融合** (#986): NPS +2.24% (10s)
- **Finny cache accumulator コピー削減** (#987): NPS +3.12% (10s)
- **StateInfo の in-place 更新** (#988): NPS +1.70% (5s)
- **千日手履歴の直接 index 化** (#989): NPS +1.22% (10s)
- **continuation correction table のキャッシュ** (#990): NPS +0.28% (10s)
- **mate1ply の gold-like piece キャッシュ再利用** (#991): NPS +1.21% (10s)
- **冗長な空 pin check の除去** (#992): NPS +0.71% (10s)
- **LayerStacks fast path の changed-index 収集 defer** (#940): NPS +2.65% (5s)
- **動的 LayerStacks (runtime-dimensional universal) の更新性能改善** (#951): 現行動的版比
  **+32.80%**、固定次元版比 -7.29% まで縮小 (WASM・1 thread・固定ノードのベンチ計測)

### NNUE モデル対応

- **LayerStacks KingRank9 bucket mode** (#936): YaneuraOu KingRank9 互換の 9-bucket 構成
- **universal edition の runtime-dimensional 化** (#950): モデルヘッダー駆動で任意次元の
  LayerStacks / HalfKX を固定 edition 列挙なしに読み込む。underscore 形式 FT ヘッダー
  (`HalfKA_hm` 等) の受理は #952
- **nnue_saturation** (#924, #925): LayerStacks 活性飽和率の実局面計測ツール

### 教師データパイプライン (dual-label PSV / gensfen)

- **dual-label PSV パイプライン**: `rescore_psv --out-scores` (i16 sidecar) と
  `psv_gate_by_king_zone --out-mask` (入玉ゲート bitmap) の出力 (#997)、CLI の base/override
  語彙統一 (#998)、生成・逆抽出・fail-closed 検証ツール `psv_dual_label` (#1000)。
  学習側 (tatara v0.7.0 の `--dual-label-psv`) と実データ 11,468,800 局面で loader batch stream
  の bit 一致を検証済み
- **shuffle_psv に段階削除フラグ** (#999): ピークディスク 3x → 2x
- **gensfen**: 数日規模の無人生成でデータを失わない resume / クラッシュ耐性 (#937、互換性変更
  は下記)、異常終局の偽勝敗ラベル排除と千日手・入玉宣言勝ちの裁定 (#935)、`--fv-scale`
  override と control.json 動的制御 (#964)、`--omit-diversions` (#966)、hcpe3 policy 分布を
  dlshogi 参照実装の既定に一致 (#973)、pack 形式の replay 整合性を hcpe3 と同等に (#974)
- **relabel_psv**: diversion の result 整合性フィルタ (drop-contaminated) (#933)
- **PSV move16 を実 YaneuraOu 形式 (A) に一本化** (#932)、hcpe → PSV 変換 `hcpe_to_psv` (#931)
- **nyugyoku_metrics**: 宣言ルール距離ペア順序一致指標 (#967)、探索読み切り詰み距離
  concordance (#972)

### 探索

- **update_all_stats の SF 追従 3 点 + LMR allNode depth スケールの tunable 化** (#927)
- **固定 depth 探索の動的 depth-liveness guard** (#938)
- **cutoff_cnt クリアの修正** (#979, #980): 探索開始時 stack[0]/[1] と毎 go の全スロット

### CSA server / client

- **入玉宣言ルールの設定駆動化** (#955, #956, #960): 24 点法 / 27 点法を env / CLI で切替、
  マッチ単位で永続化、Game_Summary で広告し client がエンジンへ自動伝達
- **player ratings API** (#948)、**D1 検索インデックスと `GET /api/v1/games/search`** (#923)
- 観戦時計の同期 (#947)、再接続時の record 保持 (#944) / clock 補償の上限 (#945)、
  DO 再起動後の cold rejoin fallback (#943)
- viewer API の R2 get N+1 並列化 (#921)、games-index backfill の並列化 (#929)
- CLOCK_PRESETS に per-preset max_moves (#942)、workers.dev / preview URL の無効化 (#957)

### Windows 対応

- ツール・テスト・ビルドの Windows 対応を一括修正 (#968, #969, #970, #971, #975, #977,
  #978, #981, #982)。`search_only_ab` の Windows PMC counting (ETW) は #995

### その他ツール

- compare_nodes にエンジン別ノード上限 (#941)、SPRT 直接対決集計の test 視点統一 (#934)、
  ONNX 使用バイナリの exit 時 abort 回避 (#926)、`--onnx-batch-size` 既定 1024 (#920, #922)、
  jsonl_to_psv の破損ログ耐性 (#965)

### rshogi-core (crates.io) / 互換性変更

- **rshogi-core 0.5.2 (crates.io)**: Linux で使用する `libc::NAME_MAX` が公開された
  `libc 0.2.186` を最小依存バージョンとして明示。既存の lockfile が古い `libc` を選択した
  consumer で 0.5.1 がコンパイルできない問題を修正。
- **rshogi-core 0.5.1 (crates.io)**: default の `edition-universal` をモデルヘッダー駆動の
  runtime-dimension構成へ変更。HalfKXの5 FT・3活性化と、LayerStacksの5 FT・任意次元・
  PSQT・full Threatを、次元ごとの固定editionを列挙せず読み込めるようにした。動的
  LayerStacksの差分更新性能を改善し、従来のPascalCase形式に加えて一般的なunderscore形式
  (`HalfKA_hm`等) のFTヘッダーも受理する。EffectBucketとThreat非fullプロファイルは未対応。
- **rshogi-core publish 規約の訂正**: v1.3.0 で追加した「engine の `vX.Y.Z` タグと同時に、
  そのタグから publish する」という規約を撤回。core 単独変更や engine 以外の変更だけを
  含む期間にも core を公開できるよう、core は engine release と独立して version bump・publish
  する。core 用の release tag は設けない。
- **rshogi-core 0.5.0 (crates.io)**: v1.3.0 で公開した 0.4.0 以降の core 変更を公開。
  `LayerStackBucketMode::KingRank9`、探索の修正・調整、NNUE changed-index 更新の高速化を含む。
  将来の NNUE architecture 追加を互換な 0.5.x update で行えるよう、公開 NNUE enum を
  `#[non_exhaustive]` に変更。
- **gensfen resume の互換性変更**: worker temp を永続 checkpoint として扱い、完了 `game_id` の
  欠番再実行、全 worker 成果物の result 境界復旧、既定毎対局 fsync、journal による冪等な複数成果物
  finalization、正常終了 worker 成果物の fail-closed 検証、final 長・staging 中 SHA-256・PSV/sidecar
  件数検査、fresh run の全 final path 上書き拒否、worker エラーの非ゼロ終了を追加。meta に native/USI
  実行ファイルと path-valued USI option を含む生成条件 fingerprint・内容 SHA-256 を記録する。従来の
  fingerprint/commit offset を持たない worker checkpoint は安全に復元できないため resume を拒否する。
  既存 temp は退避してから新規 run を開始する必要がある。
- **relabel_psv**: 処理完了時の stderr 統計を human-readable な1行から JSON 1行へ変更。

## v1.3.0 — 2026-07-11

v1.2.0 後の機能追加リリース。定跡 (opening book) 機構一式 — 新規クレート `rshogi-book`
と定跡の生成・評価・展開・逆伝播ツール群 — と、floodgate 運用のための観戦・戦績ツール
(kifu_player ライブ観戦 / live-mirror / floodgate_record)、入玉教師データ生成基盤
(gensfen 終局メタ + nyugyoku_gensfen / ek_testset / relabel_psv) が新規追加の中心。
NNUE は effect-bucket 特徴量 (推論側第一段階) と threat full-symdedup profile を追加。

`rshogi-core` 0.4.0 を crates.io へ publish した (2026-07-11)。publish 元ソースは
commit 411f7f33 = 本リリースタグの直前 commit で、タグとの差分は本 CHANGELOG 追記のみ。
過去記録の訂正 2 点: (1) v1.1.0 セクションは「0.3.0 → 0.4.0 として publish」と記載
しているが、実際は bump のみで crates.io への publish は行われておらず、0.4.0 の公開は
今回が初 (公開内容には v1.1.0〜本リリースの core 変更を含む)。(2) v1.2.0 の GitHub
Release ノートおよび release commit の「rshogi-core は v1.1.0 (0.4.0) から変更なし」は
誤りで、実際には v1.1.0→v1.2.0 でも core に変更が入っていた (バージョン番号を変更有無の
根拠にしていたことによる誤記)。冒頭の publish 規約はこれらの再発防止。

### 定跡 (opening book) 機構

- **rshogi-book クレート (Phase 1)** (#849): YANEURAOU-DB2016 形式の定跡 DB リーダと
  probe を新規追加。先後反転局面を単一エントリで共有する FlippedBook 対応。
  workspace 内クレート (crates.io へは publish しない)。
- **BookSelectValue** (#901): 評価値フィルタ生存候補から value 最大手を決定的に選択する
  USI オプションを追加 (同値は count 降順 → USI 昇順)。count 比例抽選が value 最高手を
  持ちながら劣る手を引く母集団バイアス (floodgate 実害あり) への対策。既定 false で
  既存挙動は不変。
- **BookEvalDiff の基準修正** (#892): 許容評価値差の基準を count 筆頭手でなく候補中の
  最大 value に変更。
- **定跡ツール群 (crates/tools)**:
  - `book_from_csa` (#868): CSA 棋譜コーパスから定跡 .db を生成。
  - `book_rescore` (#869, #870): 定跡 .db の各指し手に USI エンジン評価値または
    ONNX 静的評価値を付与 (journal/resume・work-stealing・決定性担保)。
  - `book_extend` (#902, #903): エンジン最善手が候補に無いノード (実測 39%) にのみ
    count=0 で bestmove を追加する展開パス。既存手は不変。--book/--out/--journal/
    --report 全ペアのパス衝突を拒否。
  - `book_backprop` (#900): 定跡 .db の評価値を negamax 逆伝播。既定 --merge min で
    下方向にのみ伝播し max 連鎖の上方バイアスを排除、千日手ループは SCC 縮約 +
    draw-value 下界の値反復で処理。
  - `book_kachi_label` (#905): (ノード, 指し手) ごとの入玉宣言決着率を棋譜コーパスから
    集計する政策層 sidecar (真値 .db と分離)。

### floodgate 運用・観戦ツール

- **kifu_player の観戦強化** (#847, #882, #887, #889, #890, #891): `--live` 追記監視
  (ライブ追従モード)・`--ratings` レート併記・`rate:`/`date:`/`sfen:` 検索・wdoor 形式
  CSA の評価値/消費時間読み取り・進行中対局の一覧表示・消費時間の秒表示など。
- **live-mirror** (#883, #904): wdoor 当日対局をローカルへミラーし kifu_player --live で
  観戦。`--push` は MONITOR2 broadcast を着手通知 (ドアベル) に、HTTP 公開 CSA を正本に
  使うハイブリッド構成で、評価値込みミラーを手単位遅延 (~1s) に短縮。TCP 断時は
  ポーリングへ自動フォールバック。
- **floodgate_record** (#877, #878, #879, #880): csa_client JSONL から 1 エンジンの
  戦績を集計。csa_client config 連動、`--fetch-ratings` による現在レート併記と
  鮮度キャッシュ・履歴記録、後手勝ち統計・負け/引分分離。
- **csa-client live_jsonl** (#884, #885): 対局中に JSONL へ手単位追記し自局を
  リアルタイム観戦 (kifu_player 連携)。rename リトライと失敗時の tmp パス明示。
- **運用スクリプト例・doc** (#886, #888, #871, #893): watch/stats/tui/rebuild スクリプト
  一式 (watch.ps1 込み)、floodgate config 完全サンプル、password の
  `<game_name>,<trip>` 形式の明記、rebuild_tools.sh の非ログインシェル対応。

### NNUE

- **effect-bucket 特徴量 (推論側第一段階)** (#907): 盤上駒の base index を物理マスの
  利き数バケットで拡張する特徴量 (`EffectBucket=` arch token、2x2/3x3 × kingfixed/
  kingbucketed config)。engine load (arch/dims/config 照合と mismatch reject)・
  full-refresh 評価・差分バケット更新・cross-repo golden dumper
  (`dump_effect_bucket_golden`) まで。accumulator は correctness 優先で常に full refresh
  (差分更新の NPS 最適化は次段階)。大 FT 対応で leb128 圧縮サイズ上限を 256MB → 2GiB に
  拡大。preset edition は 512/1024 幅の 2x2_kingfixed (#907) と 1024 幅の
  3x3_kingfixed (#908)。
- **threat full-symdedup profile (id 4)** (#899): profile 0 と同一 index 空間のまま、
  necessarily-mutual な cross-class 対称 edge の片側を列挙時に drop する compile-time
  profile。edition `…-1536x16x32-threat_symdedup` を追加。tatara 側と id/規則/index
  layout を一致させ、startpos golden で固定。
- **LayerStack size variant**: `3072x16x32` arch (#850)、preset edition
  `…-768x16x32-threat` (#863) を追加。

### 入玉教師データ生成基盤 (tools)

- **gensfen の終局メタ・来歴記録** (#906): 終局時の 27 点法点数・玉侵入・敵陣内駒数を
  JSONL result 行に記録 (裁定は不変)。random-multi-pv / random_move で PV1 以外を選んだ
  ply・手・score_gap_cp を diversions 配列に記録 (後段 deblunder 用)。
  `--emit-game-id-sidecar` で PSV レコードと 1:1 の game_id sidecar を出力 (#918)。
  native + LayerStacks (num_buckets>1) で `--progress-file` 未指定なら起動エラーにし、
  progress 重みゼロフォールバックのサイレント品質バグを修正。--resume 時はパスと
  内容 SHA-256 の両方を照合。
- **gensfen の教師ラベル裁定**: timeout・illegal_move・no_bestmove の対局を教師データから
  全局破棄し、result JSONL の `adopted` と終局理由別サマリで可視化。通常千日手と連続王手
  千日手を対局ループで裁定し、宣言勝ち局面を `move16=0` の PSV 終端局面として収録。
  MultiPV 乱択は評価値差の明示指定を必須化。
- **nyugyoku_gensfen** (#906): 入玉譜 manifest から玉の敵陣初侵入 ply にアンカーした
  開始局面集を抽出 (entry-40/-20/0/+20)。direct-mapped 固定サイズ dedup・partition 分割で
  数億局面規模でもピークメモリ入力非依存、checkpoint resume 対応。
- **ek_testset** (#909, #916): held-out floodgate 棋譜から入玉局面の評価テストセットを
  build し native NNUE で採点。DT (宣言真値: declaration_win を ground truth) と
  OC (勝敗較正: sign_acc / WDL cross-entropy / Brier / calibration) の 2 系統。
  draw を期待スコア 0.5 で算入、cp→勝率の `--scale` 既定は 600。全段 streaming。
- **relabel_psv** (#918): PSV の score を game_result 由来の飽和値へ上書きし勝敗信号
  ベース化 (λ=0 レシピでの弱評価の循環ラベル対策)。`--declaration-override` /
  `--deblunder` (game_id sidecar + diversions 来歴で乱択汚染対局を除外)。全体 streaming、
  入出力の同一実体 (symlink/hardlink 含む) を検出し原本破壊を防止。
- **eval_sfens** (#916): score_cp 列の追加と bin エントリポイントの復元。
- **core の公開 API 化** (#906): 宣言点数計算を `Position::entering_king_point_info`
  として公開し declaration_win と共有。progress 係数ローダーを core へ移動し
  usi / tools で共用。

### CSA server / client の修正

- **fischer 時計の修正** (#857〜#861): server 側 time_margin_ms を課金から外し deadline
  猶予のみに限定、client 側 btime 二重計上の解消と margin の fischer 適用、再接続時の
  台帳膨張退行の修正。
- **csa-server-workers**: cold-start の計時起点張り直しを廃止し幽霊 TimeUp を解消 +
  復元不能時の安全網 (#852/#854)、finalize の live-index delete を config 欠落に強く
  (#853/#855)、viewer 経路の DO/ライフサイクル欠陥 4 件を修正し観戦者へ評価値を配信
  (#856)、終局済み対局への spectate 初回応答と観戦 slot リーク修正 (#866)、live-orphan
  sweep の zombie 件数サマリ (#873)、結果コード契約マニフェスト + generate-and-compare
  テスト (#875)、短時間 Fischer プリセット fischer-60-5F (#851)。
- **csa-client**: floodgate へ送る PV コメントの欠落・破損を修正 (#896)。
- **csa-server-tcp**: send_line を write_all 1 回に統合 (アプリ層での分割送信を排除)
  (#897)。

### パフォーマンス / スケーラビリティ

- **rescore_psv: --search-depth / --engine のチャンクストリーミング化** (#910):
  全レコード load-all を解消しピークメモリを入力件数非依存に (100 万件チャンク方式)。
  エンジン死亡時は同一チャンク内で生存エンジンへ再割り当てし入力順を維持、全滅は
  エラー終了で部分出力を成功扱いしない。中断時の部分出力は入力の連続 prefix を保証。
  --threads 1 / 単一エンジンで旧実装と出力 bit 一致。
- **ek_testset / nyugyoku_gensfen / relabel_psv**: いずれも入力件数非依存のピーク
  メモリで設計 (上記参照)。

### その他 (fix / refactor / docs / ci)

- test(csa-client): mock USI engine 起因の flake 修正 — write/spawn fork の同一 lock
  直列化で ETXTBSY を解消 (#912)、info→bestmove 順序と終局レースの決定化 (#915)。
- ci: Test job のビルドで thin LTO を無効化しフルビルド 10m55s → 2m16s、push トリガーを
  main に限定 (#913)。worker-build の version 固定 (#872)。Security Audit
  RUSTSEC-2026-0204/0205 対応 (#898)。GitHub Actions グループ更新 (#848, #917)。
- refactor(tools): 進捗表示を tools::progress に共通化し book_rescore に進捗を追加
  (#876)。CSA replay を TUI 非依存の csa-replay feature に分離 (#909)。
- docs(tools): rescore_psv doc をユーザー導線中心に再編 (クイックスタート新設、実装詳細
  を internals へ分離) (#911)。nyugyoku_gensfen doc も同様の構成で追加 (#906)。

## v1.2.0 — 2026-07-03

v1.1.0 後の機能追加 + パフォーマンス改善リリース。教師データ品質パイプライン
(prep_hcpe / rescore_hcpe / teacher_labeler / yardstick_label・score) と棋譜レビュー用
TUI (kifu_player) が新規追加の中心。パフォーマンス面は rescore_psv / psv_to_hcpe3 の
教師データツール群で複数の高速化 (最大 ~30x) を実施。

### パフォーマンス改善

- **rescore_psv: ONNX 推論の複数セッション in-flight 多重化** (`--onnx-sessions`, #845):
  単一セッションでは H2D→compute→D2H が完全直列になり GPU アイドルが生じていた
  (nsys 実測: kernel/memcpy overlap 0ms, GPU idle 22%)。別 CUDA ストリームに multiplex
  する複数セッション供給に再構成。
  実測値: 208-213k pos/s → 237-242k pos/s (**+15%**)。GPU util 95% → 100%、消費電力
  548W → 575W (TGP 上限)。出力は sessions 数に依存せず bit 一致。
  (実測: RTX 5090 (native), TRT fp16, cached engine, 10M 局面 / commit bad207c2)

- **rescore_psv: 入力/出力 host バッファの CUDA pinned 化** (#812 相当 / 703cb185, 6bb066e5):
  pageable バッファでは `cudaMemcpyAsync` が pageable→pinned staging で実質同期化し、
  nsys で全処理の ~96% を占めていた。pinned 化で真の async 転送に変更。
  実測値 (2M records, batch1024, min-of-3): TensorRT FP16 132k → 167k pos/s (**+26%**) /
  CUDA EP FP32 45k → 54k pos/s (**+21%**)。出力バッファも同様に pinned 化 (#837)。
  (実測: RTX 5090 (WSL2) / commit 6bb066e5, 703cb185)

- **rescore_psv: ONNX producer の set_from_parts 化**（String 往復除去, #838）/
  **内部 NNUE 評価の parts 直接構築**（#815）: `unpack_sfen→String→set_sfen` の局面復元を
  `unpack_sfen_to_parts→set_from_parts` 直接構築に置換し per-record の文字列確保を排除。
  静的 NNUE 評価や producer 側の read/build 処理を大きく高速化した（出力は bit 一致を維持）。
  (commit 927863fb, 15d4273e)

- **psv_to_hcpe3: PSV→hcp 直接展開**（+ evalfix bake, #814）: convert ホットパスの
  `unpack_sfen→String→set_sfen→pack_position_hcp` 往復を排除し
  `unpack_sfen_to_parts→pack_hcp_from_parts` で直接 hcp 化。文字列往復除去により変換処理を
  大幅に高速化した（出力は bit 一致を維持）。(commit c58c59b5)

- **rescore_psv: dlshogi-ONNX 供給の GPU 推論パイプライン化**: CPU 前処理と GPU 推論を
  producer/consumer に分離しオーバーラップさせ、GPU アイドル区間を解消。
  実測値 (DL_suisho15b, BS=1024, 500k records): 60.6s → 52.2s (**-14%, pos/s +16%**)。
  GPU util 91.9% → 97.3%、boost clock 1695→1929 MHz を維持。
  (実測: RTX 3080 Ti / commit 74b7bd82)

- **threat 評価: find_usable_accumulator の遡及深さを runtime has_threat で可変化**:
  threat モデルと非 threat モデルとで最適な遡及深さが異なる非対称性を確認し、
  compile-time feature でなく runtime 分岐に変更。両モデルそれぞれで最適な深さを選択できる
  ようになった（accumulator 値は不変、bit-identical を確認）。

- **extract_bench_positions: streaming reservoir sampling 化**（メモリ入力非依存, #770）:
  全局面を Vec に蓄積してから層化サンプルする load-all 設計を Algorithm R の reservoir
  sampling に変更。ピークメモリ使用量を入力サイズに依存しない形に削減した（出力件数・
  sign_validation は旧設計と完全一致、同一 seed での決定性も維持）。

### 新機能

- **kifu_player: PSV / tournament JSONL 共通の棋譜プレイヤー TUI** (#839, #841, #842, #843):
  棋譜を盤面付きで再生・レビューする TUI を新規追加。その後 CSA 入力対応と派生指標での
  ソート・検索 (Tier1+2)、盤面/指し手表示のブラッシュアップを追加 PR で拡張。

- **教師データ品質パイプライン一式**: hcpe3 教師形式（各手に MultiPV soft policy, gensfen
  側 5a3ae197）、prep_hcpe（hcpe 教師の汚染除去・重複除去・shuffle・分割, b0f818ac）、
  共有ラベリングコア teacher_labeler + rescore_hcpe（intra-chunk resume 対応込み, ba27b160
  他）、ラベル品質「物差し」ハーネス yardstick_label/score（ONNX labeler モード・
  --capture-depths depth sweep・--spsa-params 対応込み, 37dcf9e7 他）、ベンチ局面
  ground truth ラベラー label_bench_positions / DL水匠リスコアラ label_bench_dl（#769,
  #772, #773）を新規追加。教師データの生成・清浄化・評価を一通りツール化。

- **NNUE: Threat 特徴量の拡張**: Threat cross-side profile (id10) 追加 + arch_str dims
  照合で load 堅牢化（cc032cdf）、threat profile step-attacker (slider attacker 除外,
  id3/33408) を engine に追加（7b8c5b34）。対応する preset edition
  (`edition-layerstacks-halfka_hm_merged-1024x16x32-threat`, #822) も追加。

- **NNUE: LayerStack size variant 追加**: `1024x16x32`（FT_OUT=1024, L1=16, L2=32,
  既存 const generic 流用のため inference kernel 追加なし）、`768x8x32`（#775, #776）
  の 2 size variant を追加。

- **ビルド: feature / edition 命名の de-abbreviate**: Cargo feature / preset edition 名
  から省略形 `ls-` / `ext` を排し自己説明的な名前へ移行（互換 alias なし、破壊的リネーム）。
  `ls-arch`→`layerstack-arch`、`ls-size-<dims>`→`layerstacks-<dims>`、
  `ls-ext-psqt`/`ls-ext-threat`→`nnue-psqt`/`nnue-threat`、
  `edition-ls-…`→`edition-layerstacks-…`。旧名を直接指定する build invocation は要更新。

- **tournament: per-engine `--engine-nodes`** を追加（zero-node 棄却・meta 記録込み）。

- **csa-client**: `--target` preset 接続先をカスタムドメインに変更、
  `analyze_selfplay` 互換 JSONL 出力を既定 ON 化 (#768)。

### その他 (fix / refactor / docs / ci)

- fix(csa-server): cold-start 復元後の turn alarm 誤発火・viewer キャッシュ 500 エラーを修正
  (#780, #798, 013b0d39)。
- fix(search): SE 探索爆発による fixed-depth 非終了バグを修正、rtime+depth の打ち切り予算
  除外。
- fix(nnue): AVX-512 ビルドで OUTPUT_DIM=8 の AffineTransform が誤 eval を返す不具合を修正
  (0c18dee9)。
- fix(tools): psv_to_hcpe3 の成り手 move16 変換バグ、hcpe3 policy 負け詰み符号バグ (#817)、
  floodgate URL/index 形式 rot 対応など多数の tools 側バグ修正。
- fix(deps): RUSTSEC-2026-0190 (anyhow), RUSTSEC-2026-0185 (quinn-proto) 対応。
- refactor(nnue): AffineTransform 重みレイアウト判定の一元化、LayerStack enum 整理。
- ci: GitHub Actions の commit SHA pin 化、NNUE wasm/SIMD (Intel SDE) runtime 検証 job 追加。
- build(tools): reqwest を rustls-tls 化し openssl-sys 依存を除去。
- docs: skills (教師データ一括変換・再評価、selfplay、edition-build)、csa-server/client
  運用ガイド、usi-perf-measure のハマりどころ追記など多数の doc 整備。

## v1.1.0 — 2026-06-02

v1.0.0 後の追加機能リリース。tatara 学習側の bucket 数可変化に追従し、tournament
ツールに動的制御を入れる等の運用改善が中心。同時に crates.io `rshogi-core` を
0.3.0 → 0.4.0 (semver minor bump) として publish。

### NNUE (#727 / #758, #757)

- **可変バケット数 LayerStack net 対応** (#758 / Issue #727): 学習側
  ([tatara](https://github.com/SH11235/tatara) の
  [ADR 2026-05-23 "LayerStack / progress のバケット数 (N) の可変化"](https://github.com/SH11235/tatara/blob/main/docs/decisions/2026-05-23-num-buckets-configurable.md))
  に追従し、`.bin` の新 layout を engine 側で読み込めるようにした。新 version
  `NNUE_VERSION_LAYERSTACK_NUM_BUCKETS` (`0x7AF32F21`) は `arch_str` 直後に
  `num_buckets: u32` field を持つ self-describing layout。`NNUE_VERSION_HALFKA`
  (`0x7AF32F20`) は引き続き暗黙 9 bucket の legacy compat path として load する。
  N の上限は `MAX_LAYER_STACK_BUCKETS = 16` (AVX-512 1 命令のレーン数と一致)。
  - PSQT 配列を `[i32; MAX_LAYER_STACK_BUCKETS]` 固定長 + runtime `psqt_num_buckets`
    に置き換え、SIMD path は AVX-512F (16-lane mask) / AVX2 (maskload × 2 chunk)
    / scalar fallback の三段構成で N 可変対応。
  - progress → bucket binning を `floor(sigmoid(sum) × N).clamp(0, N-1)` に
    一般化 (`progress_sum_to_bucket(sum, n)`)、N ごとの閾値を `OnceLock<Box<[f32]>>`
    で lazy 構築。
  - 非-LayerStack `NNUE_ARCHITECTURE` override で num_buckets-header net を
    読もうとした場合の早期 reject、`num_buckets > MAX` / `num_buckets == 0` の
    reject を `InvalidData` で明示。
  - NPS bench (9-bucket LayerStack + PSQT 配布 net、300k iter): 旧 9-bucket 固定
    SIMD 実装 1,169,473 evals/sec → 本リリース runtime mask SIMD 実装
    1,257,854 evals/sec、evaluate あたり 790.3 ns → 790.4 ns (退行無し)。
  - 詳細: 本リポジトリ [ADR 2026-05-26](docs/decisions/2026-05-26-variable-num-buckets-layerstack-load.md)。
- **HalfKp LayerStack の玉 BonaPiece OOR panic 修正** (#757): `layerstacks-halfkp` edition
  で ply32 前後の局面探索中に玉 BonaPiece (`≥ FE_END`) が FT 差分更新の高速経路に
  流れて panic する不具合を修正。HalfKp の `append_active_indices` /
  `append_changed_indices` の玉除外と整合を取った。

### Tools

- **tournament 実行中の動的制御** (#765 / Issue #763): 対局中に `target_games`
  と worker `concurrency` を増減できる runtime command を追加。FIFO 制御で長時間
  実行中の試合構成を再計画できる。
- **jsonl ↔ psv 変換ツール `jsonl_to_psv`** (#764): 学習側
  ([tatara](https://github.com/SH11235/tatara)) が出力する jsonl 形式の学習データ
  を psv 形式 (gensfen 由来) に逆変換する片方向 converter を追加。

### Build / xtask

- **xtask で preset edition build と engines/ 配置を自動化** (#750 / Issue #738):
  `xtask build-engines` で複数 preset edition を順次ビルドし、`engines/<edition>/`
  配下に rename 配置するパイプラインを整備。
- **engines/ 命名規則を Edition 軸前提に本格化** (#752 / Issue #739):
  従来の flavor 軸を非採用とし、Edition 軸の preset feature set を一次元として
  命名・配置する方針に統一。
- **flavor 軸を非採用として retire** (#756): 本リポジトリ
  [ADR 2026-05-24 "build edition / flavor design"](docs/decisions/2026-05-24-build-edition-flavor-design.md)
  に補記し、flavor 軸を成立させていた CFG/feature gate を物理削除。

### CSA Server / Workers

- `rshogi-csa-server` 系列のコメント整理 (#760, #761): 冗長コメントと local
  context 依存ワードを除去し、長期保守を見据えた文体に整える (機能変更無し)。

### License

- LICENSE ファイルを canonical な GPLv3 全文に更新、SPDX 表記を統一。

### Cargo.toml / 依存

- `rshogi-core` 0.3.0 → 0.4.0 (LayerStack bucket 数 API 変更を含む semver minor
  bump)。
- `rshogi-usi` の `rshogi-core` 依存 pin 0.3 → 0.4。

## v1.0.0 — 2026-05-24

GitHub 上初の正式リリース。USI engine としての対局動作 (floodgate / WCSC 系運用) が
安定稼働に到達した時点のスナップショット。同時に crates.io `rshogi-core` を
0.2.4 → 0.3.0 (semver minor bump、後述 API 変更を反映) として publish。

### 対応 NNUE アーキテクチャ

LayerStacks (LS, tatara 学習形式) と HalfKX (Simple-arch, suisho5 互換) の 2 系統に対応。

**LayerStacks 系**

- Feature Transformer 5 種類:
  - HalfKP
  - HalfKaSplit / HalfKaMerged
  - HalfKaHmSplit / HalfKaHmMerged
- L1 サイズ 4 構成:
  - 1536×16×32 / 1536×32×32 / 768×16×32 / 512×16×32
- 活性化: CReLU / SCReLU / Pairwise
- 拡張: PSQT (Piece-Square Table accumulator) / Threat (HandThreat 特徴量)
- バケット選択: `progress8kpabs` mode (YaneuraOu 互換 `progress.bin` で 8 buckets を選択。
  LS 自体は 9-bucket バンク構造で、bucket8 は現状 mode で未使用)
- preset edition feature でビルド構成を切替 (`edition-universal` / `edition-layerstacks` /
  `edition-layerstacks-{ft}-{L1}-{ext}` 等)

**HalfKX 系 (Simple-arch)**

- 5 種類の feature set: HalfKP / HalfKaSplit / HalfKaMerged / HalfKaHmSplit /
  HalfKaHmMerged
- 活性化: CReLU / SCReLU / Pairwise
- 主な L1 サイズ: 256 / 512 / 1024 / 1536
- AVX-512BW SIMD パス対応 (FT 差分更新)

**運用機能**

- プロセス間 NNUE 重み共有メモリ: 多プロセス対局時の PSS メモリを 8 プロセスで
  3780 → 2276 MB に削減
- USI オプション `EvalFile` / `FV_SCALE` / `NNUE_ARCHITECTURE` 等で実行時に切替

### 主要 engine 機能

- **探索**: Stockfish 13 系統のアルゴリズム移植 (PVS, LMR, LMP, null-move, ProbCut,
  IID, SE, history heuristics, multi-cut, futility, razoring 等)
- **TT (transposition table)**: 16-bit key、cluster-based、generation-aware
  (YaneuraOu 一致)
- **mate_1ply**: 1 手詰めの高速判定
- TT cutoff quiet bonus / small ProbCut beta が SPSA でチューニング可能
- **NNUE 評価**: 上記 NNUE アーキテクチャによる局面評価
- **incremental update + Finny Tables**: 差分更新 + KP-abs cache + PSQT cache
- **時間管理**: byoyomi / inc / time control 各種、ponder 対応 (`PonderhitHandle`)
- **WASM/WASI ターゲット対応**: SIMD128 SCReLU 実装

### CSA Server (rshogi-csa-server / rshogi-csa-server-tcp / rshogi-csa-server-workers)

- floodgate 互換 CSA protocol 実装
- 平手 / 駒落ち (初期局面パターン) / Buoy 対局 / 観戦
- 重複ログイン制御 / LOGIN handle whitelist (security hardening 済)
- x1 拡張コマンド: VERSION / HELP / WHO / LIST / SHOW
- 2 deployment target: TCP daemon / Cloudflare Workers (WASM, Durable Objects)
- Workers 側は Pulumi で IaC 化 (Cloudflare 側設定、cron 監視 Worker)

### Tools (crates/tools)

- **tournament**: USI engine 同士の対局、複数 engine 同時比較
- **analyze_selfplay**: tournament 結果から SPRT 含む post-hoc 解析
- **spsa**: SPSA 自動チューニング (本リリース内で fishtest 整合 v4 改修済)
- **bench_nnue_eval**: NNUE 推論の throughput bench
- **verify_nnue_accumulator**: refresh vs differential update 一致テスト
- **dump_psqt_stats**: quantised.bin の PSQT 統計ダンプ
- **eval_sfens**: SFEN テキストから LayerStacks NNUE で局面評価
- **gensfen / pack_to_psv / psv_to_jsonl / rescore_psv 系**: 教師局面生成と
  packed SFEN フォーマット I/O

### rshogi-core 0.3.0 (crates.io)

0.2.4 以降の public API 変更を集約。0.x 系列の minor bump として breaking change を
含む (crates.io 上の外部 user 想定はゼロのため安全に minor bump で処理)。

#### Breaking changes (API)

- **PascalCase 命名統一** (#729 / #730 / #731 / #732):
  FeatureSet enum / 関連型 alias / 構造体名 / ファイル名 / ディレクトリ名を PascalCase
  に統一。旧 alias (`parse_feature_set_from_arch` 経由) は受理可能で arch 文字列レベル
  の後方互換は維持。
- **Atomic feature の Edition 軸再編** (#736 / #741):
  旧 `feature-*` 系 atomic feature を Edition 軸 ADR
  (`docs/decisions/2026-05-24-build-edition-flavor-design.md`) に沿って再編。
  `edition-universal` / `edition-layerstacks` / `edition-layerstacks-{ft}-{L1}-{ext}` / `edition-halfkp-crelu`
  などの preset を使う運用に変更。`layerstack-arch` の意味論も再定義。
- **LayerStack network の FT generic 化** (#745):
  `NetworkLayerStacks` に FT type parameter を追加し、`LsNetByFt<FT>` で L1 軸 dispatch
  する 2-tier enum に再構成。
- **AccumulatorStackVariant の cfg gate** (#744):
  HalfKX specific preset の workaround を撤去し、active feature で variant を制御。
- **LS dispatch macro 共通化** (#749):
  `rshogi_core::nnue::ls_dispatch_ft_size!` を `#[macro_export]` で公開。tools 3 binary
  の dispatch macro を統合し、5 FT × 4 L1 の更新ポイントを 1 箇所に集約。
  同時に `#[allow(unreachable_patterns)]` 5 箇所を cfg-gated fallback に置換 (#747)。

#### 機能追加 (API)

- HalfKaMerged / HalfKaHmSplit feature set を engine に追加 (#719)
- Simple-arch 5 feature set に SCReLU / Pairwise 活性化を追加 (#721)
- arch 文字列を構造マーカで bucket-less / LayerStacks / 活性化検出 (#717)
- NNUE 重みのプロセス間共有メモリを実装 (#714)
- Finny Tables (AccCacheEntry) に PSQT accumulator を追加 (#705)
- `dump_psqt_stats` ツール (#696)
- `add/sub_psqt_weights` を SIMD 化 (NPS +3.0%) (#687)
- Threat / HandThreat 特徴量と LayerStacks 周辺改修 (#466)
- `NNUE_ARCHITECTURE` USI オプション (#437)
- PSQT ショートカット推論対応 (#436)
- `PonderhitHandle`: clone-able な ponderhit signal API (#589)

#### バグ修正

- Simple-arch SCReLU 推論の 2 バグを修正 (#723)
- `l1_sqr_clipped_relu_activation` の AVX2 i32 乗算オーバーフロー修正 (#416)
  — NNUE 評価値が崩れる重大バグ
- A-15 King 開き王手でクラッシュするバグ修正 (#432)

#### Performance

- Simple-arch FT 差分更新の sub+add 融合 fast path (#725)
- Simple-arch FT に AVX-512BW SIMD パスを追加 (#726)
- LS dispatch 経路の dead-code 検出最適化

---

### spsa CLI (v3 → v4, fishtest 整合)

fishtest 主リファレンス (`server/fishtest/spsa_handler.py` / `worker/games.py`) と
整合させる v4 改修。「パラメータは動くが棋力が下がる」報告の根本対応として SPSA
アルゴリズムの 4 つの中核バグを修正。

#### Breaking changes (CLI)

- **`--total-pairs N` (新, 必須)**: SPSA 全体の game pair 数 (= fishtest `num_iter`)。
  `total_games = 2 × N`。
- **`--batch-pairs B` (新, 既定 8)**: 1 batch あたりの game pair 数。1 batch 内で同 flip
  ベクトルで `2B` 局を消化し、batch 末で θ を 1 回更新する (k は `+= B`、fishtest
  worker の `iter += game_pairs` と等価)。
- **`--iterations` / `--games-per-iteration` (deprecated)**: 併用すると warning + 自動
  換算 (`total_pairs = gpi × iters / 2`, `batch_pairs = gpi / 2`) で 1 リリース猶予。
- **`--seeds` (削除)** / **`--parallel-seeds` (削除)**: hard error で停止する。
  multi-seed の探索は **`--seed` を変えた独立 run dir** を並列実行する運用に置き換え。
- **`--stats-aggregate-csv` / `--no-stats-aggregate-csv` (削除)**: clap で unknown
  argument エラー。複数 run の比較は外部スクリプト (pandas/awk で
  `runs/spsa_seed*/stats.csv` を concat) で行う。
- **`--seed S` (維持)**: 単一 base_seed の挙動は同じ。SPSA の RNG stream は seed と
  batch index から決定論的に生成。

#### Breaking changes (format / CSV)

- **`meta.json` `format_version` v3 → v4**: 新フィールド `total_pairs` / `batch_pairs`
  / `completed_pairs` を追加。
- **v3 silent migration**: `format_version=3` の meta は warning を出して自動 migrate
  する (`completed_iterations × batch_pairs` で `completed_pairs` を再構築)。multi-seed
  run / 奇数 `games_per_iteration` の v3 meta は schema 上自動検出できないため、最終値
  を新 run の canonical として再投入する (`crates/tools/docs/spsa_runbook.md` §10.7 参照)。
- **`stats.csv` 列変更**:
  - 撤去: `seed`
  - rename: `games` → `batch_pairs` (値の意味も「game 数」から「game pair 数」)
  - 1 batch = 1 行 (v3 までは 1 iter あたり seed 数の行が出ていた)
- **`stats_aggregate.csv` (撤去)**: 自動生成されない。
- **`state.params` / `final.params` / `values.csv` の int 値**: `42` 形式から
  `42.000000` 形式に変更。θ 内部状態を f64 のまま保持するため (v3 までの resume 経由で
  小数部が消える退行を解消)。parser は f64 なので互換あり。

#### 主要バグ修正 (SPSA)

- **B-1 paired antithetic**: pair 内 2 局で同じ start_pos を共有し、`plus_is_black` のみ
  反転。v3 では pair 内で別 start_pos を抽選していたため開局選択ノイズが完全には相殺
  されていなかった。
- **B-2 1 batch = 1 update**: 1 batch で同 flip ベクトル + 同 plus/minus 値を使い、
  batch 末で θ を 1 回更新。schedule の k 軸を「累積 game pair 数」に再定義。
- **B-3 stochastic rounding + RNG stream 分離**: is_int 型 SPSA param の θ 内部状態を
  f64 のまま保持し、engine 送信時のみ `floor(v + U(0,1))` で確率的丸め。clamp → round
  → 再 clamp で範囲外滑り込みを吸収。RNG stream を flip / rounding / startpos 用に
  salt XOR で分離。**棋力低下の主因への根本対応**。
- **B-4 ponder=off / NetworkDelay=0 強制**: 既存実装で固定済み (確認のみ)。
- **B-5 multi-seed 機能の全廃**: `SeedRunContext` / `SeedGameStats` /
  `AggregateIterationStats` / `stats_aggregate.csv` / `resolve_seeds` /
  `mean_and_variance` / `panic_payload_to_string` を削除。

#### 関連 PR (spsa v4)

- `feat(spsa)!: fishtest 整合の v4 改修 (paired antithetic / stochastic rounding / multi-seed 撤去)` (#604)

### tournament CLI / spsa CLI (2026-04 系)

#### tournament

- `--engine-usi-option` はデフォルトで共通 `--usi-option` にマージし、同じキーは engine
  個別指定が上書きするように変更。旧挙動の完全置換が必要な場合は
  `--strict-engine-usi-option` を指定する。
- engine read timeout 時に、EvalFile 未指定・NNUE 読み込み遅延・isready 中の panic を
  疑うヒントと、取得できた engine stderr の直近行を出すようにした。

#### spsa (safety / observability series)

- `--params <path>` を完全削除 (deprecation alias なし)。代替は `--run-dir <dir>`。
  run-dir 配下に固定レイアウトで派生ファイルを配置する (#579)
- `--init-from` の暗黙スキップを禁止。既存 state がある状態で `--init-from` を指定すると
  `--resume` または `--force-init` が必須 (#576)
- `meta.json` format_version を 3 に bump。旧形式の meta は再開不可 (#576)
- 起動時に `=== SPSA Startup Summary ===` を stderr に出力 (init mode と active params
  上位 5 件を確認できる) (#577)
- `iter 0 snapshot` を `values.csv` に記録するように変更 (#577)
- `rshogi_to_yo_params`: rshogi default 値の混入を 95% 一致閾値で検知し warn/error。
  `--allow-rshogi-defaults` / `--strict-rshogi-defaults` を新設 (#578)
- `<run-dir>/.lock` で同 run-dir の二重起動を排他制御。残留 lock は `--force-unlock` で
  削除 (#580)
- 既存 state.params + フラグなし起動を bail に変更。canonical なしで既存 state を起点
  にしたい場合は `--use-existing-state-as-init` を明示指定 (silent fresh start は事故の
  温床だったため) (#580)
- `meta.json` format_version 3 → 4。`current_params_sha256` を追加し、resume 時に
  on-disk state.params の hash と meta が一致しなければ bail (#580)
- SPSA 正常完了時に `<run-dir>/final.params` を atomic に書き出し。`tune.py apply` には
  `state.params` ではなく `final.params` を渡すこと (#580)

#### ファイル名 / パスの移行表

| 旧 | 新 (run-dir 直下) |
|---|---|
| `<run>/tuned.params` | `<run>/state.params` |
| `<run>/tuned.params.meta.json` | `<run>/meta.json` |
| `<run>/tuned.params.values.csv` | `<run>/values.csv` |
| `<run>/tuned.params.stats.csv` | `<run>/stats.csv` |
| `<run>/tuned.params.stats_aggregate.csv` | `<run>/stats_aggregate.csv` |

#### CLI 移行表

| 旧 | 新 |
|---|---|
| `spsa --params RUN/tuned.params --init-from CANON ...` | `spsa --run-dir RUN --init-from CANON ...` |
| (resume) `spsa --params RUN/tuned.params --resume ...` | `spsa --run-dir RUN --resume ...` |
| (やり直し) `rm -rf RUN && spsa --params ... --init-from ...` | `spsa --run-dir RUN --init-from CANON --force-init ...` |

#### 移行チェックリスト

既存運用スクリプトをこのリポジトリ外で持っているなら、以下のパターンを grep:

```bash
rg 'tuned\.params|--params |\.values\.csv|\.stats\.csv|\.stats_aggregate\.csv|\.meta\.json'
```

```bash
rg '\-\-seeds\b|\-\-parallel\-seeds|\-\-games\-per\-iteration|\-\-iterations |stats_aggregate\.csv|stats-aggregate-csv'
```

旧 run dir からの継続は不可 (`tuned.params` は新 run の `--init-from` に渡し fresh
start で seed として再利用する。詳細は `crates/tools/docs/spsa_runbook.md` §10.7 参照)。

#### 関連 PR (spsa safety / observability)

- #576 — safety core (state machine, force-init, meta v3, atomic I/O)
- #577 — observability (iter 0, startup summary, stderr 統一)
- #578 — `rshogi_to_yo_params` の default 検知
- #579 — `--params` 廃止 + `--run-dir` 採用 + ドキュメント整理
- #580 — checkpoint safety (lock + state hash + use-existing 明示化 + final.params)
- #581 — runbook §10.7 命名整理 + run-dir integration test (fake USI engine)

---

### Archive tag (release ではない)

- `archive/nnue-unadopted-features-20260415` — 採用しなかった NNUE 実験を保管する archive。
  release tag ではない。
