# Changelog

リリースごとの主要変更点と移行手順をまとめる。詳細は各 PR / runbook を参照。

GitHub Release tag は engine 全体 (USI engine + CSA server + tools + spsa) の release
marker として `vX.Y.Z` を打つ。crates.io 上の `rshogi-core` は別系列 (0.x semver) で
運用しており、library API の互換性は core のバージョンで判断する
(`crates/rshogi-core/Cargo.toml`)。

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
- **HalfKp LayerStack の玉 BonaPiece OOR panic 修正** (#757): `ls-halfkp` edition
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
- preset edition feature でビルド構成を切替 (`edition-universal` / `edition-ls` /
  `edition-ls-{ft}-{L1}-{ext}` 等)

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
  `edition-universal` / `edition-ls` / `edition-ls-{ft}-{L1}-{ext}` / `edition-halfkp-crelu`
  などの preset を使う運用に変更。`ls-arch` の意味論も再定義。
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
