# ランタイム環境変数ガイド（engine-core / engine-usi）

このドキュメントは、`packages/rust-core` の Rust エンジンが解釈する**環境変数ベースのランタイムトグル**をまとめたものです。  
USI `setoption` ではなく、プロセス起動時の環境変数で挙動を調整したいときのリファレンスとして使ってください。

- 対象: `engine-core` / `engine-usi` / 並列探索 / 一部ツール（`crates/tools`）  
- 形式: 変数名 / 用途 / 既定値（コード上のデフォルト）を簡潔に列挙

> 注記: 多くのパラメータは USI オプション（`SearchParams.*` 等）からも設定できます。  
> 環境変数は主に「一時的な実験」「CI やベンチ環境での切り替え」に向けた低優先度の入口です。

---

## 1. 探索・時間管理まわりのトグル

### 1.1 安定化ゲート / 近締切ゼロ窓検証

`crates/engine-core/src/search/ab/driver.rs` で参照される探索安定化系のスイッチです。

| 変数名 | 型 / 既定値 | 用途 |
|--------|------------|------|
| `SHOGI_DISABLE_STABILIZATION` | bool, 既定 `false` | 近締切ゲート / アスピ安定化 / 近終局ゼロ窓検証など「安定化ゲート」全体を一括無効化（旧名 `SHOGI_DISABLE_P1` も互換）。 |
| `SHOGI_LEAD_WINDOW_FINALIZE` | bool, 既定 `true` | リードウィンドウ（Soft 期限）到達時に `Finalize(Planned)` を送るかどうか。`off`/`0`/`false` で無効化。 |
| `SHOGI_LEAD_WINDOW_MS` | u64, 既定 `10` | 〆切に向かう「リードウィンドウ」幅（ms）。大きくするほど余裕を持って停止に入る。 |
| `SHOGI_QNODES_LIMIT_RELAX_MULT` | u64, 既定 `1`（1..32） | qsearch の上限 `DEFAULT_QNODES_LIMIT` に対する緩和倍率。長時間・分析モードで seldepth を伸ばしたいときに >1 を指定。 |

近締切ゼロ窓検証（`near_final_zero_window_*`）関連:

| 変数名 | 型 / 既定値 | 用途 |
|--------|------------|------|
| `SHOGI_ZERO_WINDOW_FINALIZE_NEAR_DEADLINE` | bool, 既定 `false` | 近締切帯で PV1 を狭窓（ゼロウィンドウ）で 1 回だけ検証し、Exact を確認するかどうか。 |
| `SHOGI_ZERO_WINDOW_FINALIZE_BUDGET_MS` | u64, 既定 `80`（10..200） | 上記検証に割り当てる最大時間（ms）。内部で qnodes 上限に変換される。 |
| `SHOGI_ZERO_WINDOW_FINALIZE_MIN_DEPTH` | i32, 既定 `4`（1..64） | 検証を行う最小反復深さ。浅い反復ではスキップ。 |
| `SHOGI_ZERO_WINDOW_FINALIZE_MIN_TREM_MS` | u64, 既定 `60`（5..500） | 残り時間がこの値未満なら検証を行わない。極小残り時間での回し直し抑止。 |
| `SHOGI_ZERO_WINDOW_FINALIZE_MIN_MULTIPV` | u8, 既定 `0` | MultiPV がこの値未満のときは検証を行わない。高 MultiPV 時のみ ON にしたい場合に利用。 |
| `SHOGI_ZERO_WINDOW_FINALIZE_VERIFY_DELTA_CP` | i32, 既定 `1`（1..32） | ゼロ窓検証の評価差許容幅（cp）。 |
| `SHOGI_ZERO_WINDOW_FINALIZE_SKIP_MATE` | bool, 既定 `false` | mate 近傍では検証自体をスキップするかどうか。 |
| `SHOGI_ZERO_WINDOW_FINALIZE_MATE_DELTA_CP` | i32, 既定 `0`（0..32） | mate 近傍で Δ を追加拡張する幅。 |
| `SHOGI_ZERO_WINDOW_FINALIZE_BOUND_SLACK_CP` | i32, 既定 `0`（0..64） | 探索境界に対する緩衝幅。ゼロ窓検証の安全側マージン調整用。 |

### 1.2 MultiPV / 浅層ゲート

`crates/engine-core/src/search/ab/driver.rs` / `src/search/params.rs` で使用。

| 変数名 | 型 / 既定値 | 用途 |
|--------|------------|------|
| `SHOGI_MULTIPV_SCHEDULER` | bool, 既定 `false` | MultiPV 時に PV1 を優先し、PV2 以降の qsearch 予算を強く絞るスケジューラを有効化。 |
| `SHOGI_MULTIPV_SCHEDULER_PV2_DIV` | u64, 既定 `4`（2..32） | PV2 以降に対する qsearch 予算の分配倍率。大きくすると PV1 偏重が強まる。 |
| `SEARCH_SHALLOW_GATE` | bool, 既定 `false` | 浅い深さ（d≤`SEARCH_SHALLOW_GATE_DEPTH`）で ProbCut / NMP を抑制し、LMR を弱める「浅層ゲート」を有効化。 |
| `SEARCH_SHALLOW_GATE_DEPTH` | i32, 既定 `3`（1..8） | 浅層ゲートを適用する最大深さ。 |
| `SEARCH_SHALLOW_LMR_FACTOR_X100` | u32, 既定 `120`（50..400） | 浅層での LMR 係数（%）。大きくすると減深が弱まり、安定寄りになる。 |

---

## 2. 探索ポリシーとプルーニング関連

これらは `crates/engine-core/src/search/policy.rs` に集約されています。多くは USI `SearchParams.*` でも上書き可能です。

### 2.1 SEE / Futility / NMP / Razoring 等

| 変数名 | 型 / 既定値 | 用途 |
|--------|------------|------|
| `SHOGI_QUIET_SEE_GUARD` | bool, 既定 ON | 静かな手に対する SEE ガード（静的交換評価による枝刈り）を有効化/無効化。`0`/`false`/`off` で無効。 |
| `SHOGI_CAPTURE_FUT_ENABLE` | bool, 既定 ON | 取り合いに対する Futility 的枝刈りを有効化。`0`/`false`/`off` で無効。 |
| `SHOGI_CAPTURE_FUT_SCALE` | i32, 既定 `75`（25..150） | Capture Futility のマージン倍率（％）。 |
| `SEARCH_NMP_VERIFY_ENABLED` | bool, 既定 ON | NMP（Null Move Pruning）の確認探索を有効/無効化。`0`/`false`/`off` で無効。 |
| `SEARCH_NMP_VERIFY_MIN_DEPTH` | i32, 既定 `16`（2..64） | NMP 確認探索を行う最小深さ。 |

### 2.2 Singular / Helper Aspiration / TT 関連

| 変数名 | 型 / 既定値 | 用途 |
|--------|------------|------|
| `SHOGI_SINGULAR_ENABLE` | bool, 既定 OFF | Singular Extension を有効化。`1`/`true`/`on` のとき ON。 |
| `SHOGI_SINGULAR_MIN_DEPTH` | i32, 既定 `6`（2..64） | Singular Extension を行う最小深さ。 |
| `SHOGI_SINGULAR_MARGIN_BASE` | i32, 既定 `56`（0..512） | Singular 判定のベースマージン（cp）。 |
| `SHOGI_SINGULAR_MARGIN_SCALE_PCT` | i32, 既定 `70`（10..300） | 深さに応じたマージンスケール（％）。 |
| `SHOGI_HELPER_ASP_MODE` | `off`/その他, 既定 `wide` | 並列探索の Helper スレッド用アスピレーションモード。`off` または `0` で無効、それ以外で Wide モード。 |
| `SHOGI_HELPER_ASP_DELTA` | i32, 既定 `350`（50..600） | Helper 用アスピ窓の幅（cp）。 |
| `SHOGI_TT_SUPPRESS_BELOW_DEPTH` | i32, 既定 `-1`（<0 で無効） | 指定深さ未満で TT 書き込み/参照を抑制したいときに使用（実験用）。 |
| `SHOGI_TT_PREFETCH` | bool, 既定 ON | TT プリフェッチを有効化/無効化（`0`/`false`/`off` で無効）。 |

### 2.3 アスピレーション失敗の扱い / ベンチ用オプション

| 変数名 | 型 / 既定値 | 用途 |
|--------|------------|------|
| `SHOGI_ASP_FAILLOW_PCT` | i32, 既定 `33`（10..200） | アスピレーション失敗（低側）の再探索窓の拡大率（％）。 |
| `SHOGI_ASP_FAILHIGH_PCT` | i32, 既定 `33`（10..200） | アスピ失敗（高側）の再探索窓の拡大率（％）。 |
| `SHOGI_PAR_BENCH_ALLRUN` | bool, 既定 OFF | ベンチモードで Primary 完了後も Helper スレッドを最後まで走らせる（再現性向上）。 |
| `SHOGI_BENCH_STOP_ON_MATE` | bool, 既定 ON | ベンチ中に Mate を検出したら即座に終了するかどうか。`0`/`false`/`off` で無効。 |
| `SHOGI_PAR_BENCH_JOIN_TIMEOUT_MS` | u64, 既定 None | ベンチ終了時に Helper を join する際のタイムアウト（ms）。0/未設定で自動設定または 3000ms。 |
| `SHOGI_STOP_DRAIN_MS` | u64, 既定 `45`（上限 5000） | 通常対局で停止要求後に「ドレイン」待ちへ使う最大時間（ms）。`0` でドレイン無し。 |
| `SHOGI_PAR_CANCEL_ON_PRIMARY` | bool, 既定 OFF | Primary 完了時に Helper を即キャンセルするポリシーを有効化。 |

---

## 3. 並列探索・スレッドまわり

`crates/engine-core/src/search/parallel/{mod.rs,thread_pool.rs}` などで参照されます。

| 変数名 | 型 / 既定値 | 用途 |
|--------|------------|------|
| `SHOGI_WORKER_STACK_MB` | usize, 既定 OS | Helper スレッドのスタックサイズ（MB）。深い再帰を多用する診断ビルド向け。 |
| `SHOGI_THREADPOOL_METRICS` | `"1"` で有効 | スレッドプール終了時にジョブ数やアイドル回数などのメトリクスをログ出力。 |
| `SHOGI_THREADPOOL_BIASED` | 任意文字列, 既定 None | 一部の実験用。設定時に biased スケジューラを選択（コード参照）。 |
| `SHOGI_CURRMOVE_THROTTLE_MS` | u64, 既定 `100` | `currmove/currmovenumber` USI 出力イベントの最小間隔（ms）。 |
| `SHOGI_TEST_FORCE_JITTER` | `"0"` で無効、既定 ON | 並列探索の RootJitter をテスト用に強制 ON/OFF。ベンチ中（`bench_allrun`）は自動的に無効化。 |
| `SHOGI_HELPER_SNAPSHOT_MIN_DEPTH` | i32, 既定 なし | Helper スレッドのスナップショット出力を始める最小深さ（内部診断用）。 |

`currmove` 出力の有効化は以下の変数とも連動します（次節参照）。

---

## 4. 出力・バックエンド選択・SIMD 関連

### 4.1 USI 出力 / バックエンド選択

| 変数名 | 型 / 既定値 | 用途 |
|--------|------------|------|
| `SHOGI_INFO_CURRMOVE` | bool, 既定 OFF | `currmove` / `currmovenumber` の USI 出力を有効化。`1`/`true`/`on` で ON。 |
| `SHOGI_FORCE_CLASSIC_BACKEND` | 任意, 既定 None | 新しいバックエンドではなく、旧 ClassicBackend を強制的に使用する緊急ロールバック用スイッチ。 |

### 4.2 SIMD / NNUE 実装切り替え

`engine-usi` と `engine-core::evaluation::nnue::simd` で参照。

| 変数名 | 型 / 既定値 | 用途 |
|--------|------------|------|
| `SHOGI_SIMD_MAX` | 文字列, 既定 `auto` | コア SIMD 実装の上限。`sse2`/`sse4.1`/`avx2` などを指定して上限をクランプ。 |
| `SHOGI_NNUE_SIMD` | 文字列, 既定 `auto` | NNUE 部分の SIMD 実装上限。`avx2`/`sse41` などを指定。 |

---

## 5. クイックサーチ / 王手関連の特殊トグル

| 変数名 | 型 / 既定値 | 用途 |
|--------|------------|------|
| `SHOGI_QS_DISABLE_CHECKS` | `"1"` で Off | `finalize_diag` 等で、qsearch 中の王手手を無効化した状態の統計を取るためのスイッチ（診断用）。 |
| `SHOGI_KING_QUIET_PENALTY` | i32, 既定コード参照 | 王周りの静かな手の減点調整用（実験用）。 |
| `SHOGI_PVEXTRACT_CAP_MS` | u64, 既定コード参照 | `pv_extract` モジュールでの PV 抽出に使う時間上限（ms）。 |
| `SHOGI_PANIC_ON_KING_CAPTURE` | bool, 既定 OFF | 不正局面（玉取り）を検出した際に panic させるテスト用スイッチ。 |

`SHOGI_PANIC_ON_KING_CAPTURE` は `position/legality.rs` や 探索中のガード (`move_picker.rs`) でも参照されます。  
通常運用では設定せず、バグ追跡や bisect 用の補助としてのみ使ってください。

---

## 6. ベンチ・テスト・補助ツール向け環境変数

### 6.1 ベンチマーク / 開発用

| 変数名 | 用途 |
|--------|------|
| `BENCH_*` 一式 | `crates/engine-core/benches/tt_collision_cluster_bench.rs` で TT サイズ / サンプル数 / プレフィルなどを調整するために使用。通常の対局とは無関係。 |
| `USI_TEST_GO_PANIC` | `engine-usi` の `go` ハンドラでテスト用 panic を発生させるスイッチ。`1` で有効化。 |
| `DIAG_ABORT_ON_WARN` | `bisect_illegal_king` など、一部ツールで警告を即 abort に格上げしたいときに使用。 |

### 6.2 メタ情報・NNUE データ生成

`crates/tools/src/bin/generate_nnue_training_data.rs` などでメタ情報として埋め込まれます。

| 変数名 | 用途 |
|--------|------|
| `ENGINE_SEMVER` | エンジンのバージョン文字列（SemVer）の外部指定。 |
| `ENGINE_COMMIT` | ソースの git コミットハッシュを明示指定したい場合に使用。 |
| `GIT_COMMIT_HASH` | 上記が無いときのフォールバックとして参照。 |

---

## 7. その他

ここに挙げた以外にも、テスト専用・一時的な実験用の環境変数がコード内に存在する場合があります。  
ただし、一般的な用途（対局・自己対局・外部ログ解析）で必要になるランタイムトグルは、ほぼこのページと
`docs/parallel-search-architecture.md` でカバーされています。

- 並列探索に特化した詳細は: `docs/parallel-search-architecture.md` の「環境変数による動作カスタマイズ」参照。
- Selfplay / ブランダー分析ワークフローは: `docs/selfplay-basic-analysis.md`
- 外部 USI ログからのターゲット生成と A/B 指標は: `docs/log-analysis-guide.md`

