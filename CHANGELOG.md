# Changelog

破壊的変更と移行手順をまとめる。詳細は各 PR / runbook を参照。

## Unreleased — 2026-05 SPSA v4 (fishtest 整合)

fishtest 主リファレンス (`server/fishtest/spsa_handler.py` /
`worker/games.py`) と整合させる v4 改修。「パラメータは動くが棋力が下がる」
報告に対する根本対応として、SPSA アルゴリズムの 4 つの中核バグを修正する。

### Breaking changes (CLI)

- **`--total-pairs N` (新, 必須)**: SPSA 全体の game pair 数 (= fishtest
  `num_iter`)。`total_games = 2 × N`。
- **`--batch-pairs B` (新, 既定 8)**: 1 batch あたりの game pair 数。1 batch
  内で同 flip ベクトルで `2B` 局を消化し、batch 末で θ を 1 回更新する
  (k は `+= B`、fishtest worker の `iter += game_pairs` と等価)。
- **`--iterations` / `--games-per-iteration` (deprecated)**: 併用すると
  warning + 自動換算 (`total_pairs = gpi × iters / 2`,
  `batch_pairs = gpi / 2`) で 1 リリース猶予。
- **`--seeds` (削除)** / **`--parallel-seeds` (削除)**: hard error で停止する。
  multi-seed の探索は **`--seed` を変えた独立 run dir** を並列実行する運用に
  置き換え。
- **`--stats-aggregate-csv` / `--no-stats-aggregate-csv` (削除)**: clap で
  unknown argument エラー。複数 run の比較は外部スクリプト (pandas/awk で
  `runs/spsa_seed*/stats.csv` を concat) で行う。
- **`--seed S` (維持)**: 単一 base_seed の挙動は同じ。SPSA の RNG stream は
  seed と batch index から決定論的に生成。

### Breaking changes (format / CSV)

- **`meta.json` `format_version` v3 → v4**: 新フィールド `total_pairs` /
  `batch_pairs` / `completed_pairs` を追加。
- **v3 silent migration**: `format_version=3` の meta は warning を出して
  自動 migrate する (`completed_iterations × batch_pairs` で `completed_pairs`
  を再構築)。multi-seed run / 奇数 `games_per_iteration` の v3 meta は
  schema 上自動検出できないため、最終値を新 run の canonical として再投入する
  (`crates/tools/docs/spsa_runbook.md` §10.7 参照)。
- **`stats.csv` 列変更**:
  - 撤去: `seed`
  - rename: `games` → `batch_pairs` (値の意味も「game 数」から「game pair 数」)
  - 1 batch = 1 行 (v3 までは 1 iter あたり seed 数の行が出ていた)
- **`stats_aggregate.csv` (撤去)**: 自動生成されない。
- **`state.params` / `final.params` / `values.csv` の int 値**: `42` 形式から
  `42.000000` 形式に変更。θ 内部状態を f64 のまま保持するため (v3 までの
  resume 経由で小数部が消える退行を解消)。parser は f64 なので互換あり。

### 主要バグ修正

- **B-1 paired antithetic** (#TBD): pair 内 2 局で同じ start_pos を共有し、
  `plus_is_black` のみ反転。v3 では pair 内で別 start_pos を抽選していたため
  開局選択ノイズが完全には相殺されていなかった。
- **B-2 1 batch = 1 update**: 1 batch で同 flip ベクトル + 同 plus/minus 値を
  使い、batch 末で θ を 1 回更新。schedule の k 軸を「累積 game pair 数」に
  再定義。
- **B-3 stochastic rounding + RNG stream 分離**: is_int 型 SPSA param の θ
  内部状態を f64 のまま保持し、engine 送信時のみ `floor(v + U(0,1))` で
  確率的丸め。clamp → round → 再 clamp で範囲外滑り込みを吸収。RNG stream を
  flip / rounding / startpos 用に salt XOR で分離。**棋力低下の主因への
  根本対応**。
- **B-4 ponder=off / NetworkDelay=0 強制**: 既存実装で固定済み (確認のみ)。
- **B-5 multi-seed 機能の全廃**: `SeedRunContext` / `SeedGameStats` /
  `AggregateIterationStats` / `stats_aggregate.csv` / `resolve_seeds` /
  `mean_and_variance` / `panic_payload_to_string` を削除。

### 移行チェックリスト

既存運用スクリプトをこのリポジトリ外で持っているなら、以下のパターンを grep:

```bash
rg '\-\-seeds\b|\-\-parallel\-seeds|\-\-games\-per\-iteration|\-\-iterations \
   |stats_aggregate\.csv|stats-aggregate-csv'
```

### 関連 PR

- `feat(spsa)!: fishtest 整合の v4 改修 (paired antithetic / stochastic rounding / multi-seed 撤去)`

## Unreleased — 2026-04 SPSA 系破壊的変更

### spsa CLI

- `--params <path>` を完全削除 (deprecation alias なし)。代替は `--run-dir <dir>`。
  run-dir 配下に固定レイアウトで派生ファイルを配置する (#579)
- `--init-from` の暗黙スキップを禁止。既存 state がある状態で `--init-from`
  を指定すると `--resume` または `--force-init` が必須 (#576)
- `meta.json` format_version を 3 に bump。旧形式の meta は再開不可 (#576)
- 起動時に `=== SPSA Startup Summary ===` を stderr に出力 (init mode と
  active params 上位 5 件を確認できる) (#577)
- `iter 0 snapshot` を `values.csv` に記録するように変更 (#577)
- `rshogi_to_yo_params`: rshogi default 値の混入を 95% 一致閾値で検知し
  warn/error。`--allow-rshogi-defaults` / `--strict-rshogi-defaults` を新設 (#578)
- `<run-dir>/.lock` で同 run-dir の二重起動を排他制御。残留 lock は
  `--force-unlock` で削除 (#580)
- 既存 state.params + フラグなし起動を bail に変更。canonical なしで
  既存 state を起点にしたい場合は `--use-existing-state-as-init` を明示指定
  (silent fresh start は事故の温床だったため) (#580)
- `meta.json` format_version 3 → 4。`current_params_sha256` を追加し、resume
  時に on-disk state.params の hash と meta が一致しなければ bail (write_params
  → save_meta の transactional 復旧検証) (#580)
- SPSA 正常完了時に `<run-dir>/final.params` を atomic に書き出し。
  `tune.py apply` には `state.params` ではなく `final.params` を渡すこと (#580)

### ファイル名 / パスの移行表

| 旧 | 新 (run-dir 直下) |
|---|---|
| `<run>/tuned.params` | `<run>/state.params` |
| `<run>/tuned.params.meta.json` | `<run>/meta.json` |
| `<run>/tuned.params.values.csv` | `<run>/values.csv` |
| `<run>/tuned.params.stats.csv` | `<run>/stats.csv` |
| `<run>/tuned.params.stats_aggregate.csv` | `<run>/stats_aggregate.csv` |

### CLI 移行表

| 旧 | 新 |
|---|---|
| `spsa --params RUN/tuned.params --init-from CANON ...` | `spsa --run-dir RUN --init-from CANON ...` |
| (resume) `spsa --params RUN/tuned.params --resume ...` | `spsa --run-dir RUN --resume ...` |
| (やり直し) `rm -rf RUN && spsa --params ... --init-from ...` | `spsa --run-dir RUN --init-from CANON --force-init ...` |

### 移行チェックリスト

既存運用スクリプトをこのリポジトリ外で持っているなら、以下のパターンを grep:

```bash
rg 'tuned\.params|--params |\.values\.csv|\.stats\.csv|\.stats_aggregate\.csv|\.meta\.json'
```

旧 run dir からの継続は不可 (`tuned.params` は新 run の `--init-from` に渡し
fresh start で seed として再利用する。詳細は `crates/tools/docs/spsa_runbook.md`
§10.7 参照)。

### 関連 PR

- #576 — safety core (state machine, force-init, meta v3, atomic I/O)
- #577 — observability (iter 0, startup summary, stderr 統一)
- #578 — `rshogi_to_yo_params` の default 検知
- #579 — `--params` 廃止 + `--run-dir` 採用 + ドキュメント整理
- #580 — checkpoint safety (lock + state hash + use-existing 明示化 + final.params)
- #581 — runbook §10.7 命名整理 + run-dir integration test (fake USI engine)
