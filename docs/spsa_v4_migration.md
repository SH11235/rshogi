# SPSA tuner v4 移行ガイド

rshogi の SPSA tuner (`crates/tools/src/bin/spsa.rs`) を fishtest 主リファレンス
(`server/fishtest/spsa_handler.py` / `worker/games.py`) と整合させる v4 改修の
移行手順を記す。

## 何が変わったか

### 1. paired antithetic (B-1)

v3 では 1 iteration 内の各 game ごとに **独立に** start position を sampling し、
`idx % 2 == 0` で先後を交互に振っていた。これだと「pair 内の 2 局で別の局面が
選ばれる」ため、開局選択ノイズが完全には相殺されていなかった。

v4 では `pair_count = games_per_iteration / 2` 個の start position を選び、
各 pair で **同じ start_pos を 2 局連続 (先後入替)** で消化する。fishtest worker
の `play_pair` と等価な動作。

### 2. 1 batch = 1 update、k 進行を fishtest 流に統一 (B-2)

v3 の主役 CLI は `--iterations N` + `--games-per-iteration M`。1 iter で M 局
消化し、iter 末で θ を 1 回更新、`k += 1` だった。

v4 の主役 CLI は `--total-pairs N` + `--batch-pairs B`。

- 1 batch = 1 flip ベクトル
- 1 batch 内で `B` 個の game pair (= `2B` 局) を実行 (全 game pair で同 flip)
- batch 末で θ を 1 回更新、`k += B` (fishtest の `iter += game_pairs` と等価)
- 終了条件: `total_pairs` 個の game pair を消化したとき (`total_games = 2 × N`)

schedule の `k` 軸は **累積 game pair 数** に再定義された。c_0 / a_0 等の式は
v3 と同じだが、`N`(= total iter) を `total_pairs` に再解釈する。

### 3. stochastic rounding + RNG stream 分離 (B-3)

v3 では `is_int` 型の SPSA param は θ 更新時に毎回 `value.round()` していた。
1 iter で 0.5 未満の更新は連続消失して棋力低下の主因だった。

v4 では:

- θ 内部状態は `is_int` でも f64 のまま保持
- engine への setoption 送信時のみ stochastic rounding (`floor(v + U(0,1))`)
- `clamp → round → 再 clamp` の順序で範囲外滑り込みを吸収
- RNG stream 分離: `flip_rng` (Bernoulli ±1) と `rounding_rng` (整数化) を
  独立 `ChaCha8Rng` で生成。base_seed に salt (`FLIP_RNG_SALT` /
  `ROUNDING_RNG_SALT`) を XOR

`state.params` (および `final.params`) も is_int 関係なく `{:.6}` 固定桁で
保存するようになった。これにより resume 経由でも小数部が保たれる。

### 4. multi-seed 機能の全廃 (B-5)

v3 の `--seeds 1,2,3` / `--parallel-seeds` は v4 で **撤去**。これらの CLI を
指定すると hard error + この移行ガイドへの案内を出して停止する。

代替: 複数 base seed の探索は `--seed` を変えた **独立 run dir** で並列実行する。

```bash
# v3
spsa --run-dir runs/spsa --seeds 1,2,3 --parallel-seeds ...

# v4
for s in 1 2 3; do
  spsa --run-dir runs/spsa_seed${s} --seed $s ... &
done
wait
```

`stats_aggregate.csv` (seed 横断集計 CSV) も削除された。複数 run の比較は
外部スクリプト (例: `pandas` で `runs/spsa_seed*/stats.csv` を concat) で行う。

## CLI 変更点

| v3                                | v4                                              | 備考                                  |
| --------------------------------- | ----------------------------------------------- | ------------------------------------- |
| `--iterations N`                  | `--total-pairs N` (新) / 旧 CLI は warning 経由 | 主役 CLI 変更                         |
| `--games-per-iteration M`         | `--batch-pairs B` (新)                          | `B = M / 2` 相当                      |
| `--seeds 1,2,3`                   | (削除) hard error                               | 独立 run dir で代替                   |
| `--parallel-seeds`                | (削除) hard error                               | 同上                                  |
| `--stats-aggregate-csv PATH`      | (削除) clap で unknown argument エラー          | 自動化スクリプトの flag 削除が必要    |
| `--no-stats-aggregate-csv`        | (削除) 同上                                      | 同上                                  |
| `--seed S`                        | `--seed S` (維持)                               | 単一 base_seed の挙動は同じ           |

deprecated 経路: `--games-per-iteration M --iterations N` を併用すると、warning を
出して `--total-pairs (M*N/2) --batch-pairs (M/2)` に **自動換算** して続行する
(1 リリース猶予)。新規 run では `--total-pairs` / `--batch-pairs` を使うこと。

## stats.csv カラム変更

v3:

```
iteration,seed,games,plus_wins,minus_wins,draws,raw_result,active_params,avg_abs_shift,updated_params,avg_abs_update,max_abs_update,total_games
```

v4:

```
iteration,batch_pairs,plus_wins,minus_wins,draws,raw_result,active_params,avg_abs_shift,updated_params,avg_abs_update,max_abs_update,total_games
```

差分:

- `seed` カラム削除 (multi-seed 廃止)
- `games` → `batch_pairs` (1 batch あたりの game pair 数。値は `games / 2`)
- 1 batch = 1 行 (v3 では 1 iter あたり seed 数の行が出ていた)

## resume 互換性 (format_version v3 → v4)

v3 形式の `meta.json` を v4 で読むときの挙動:

### silent migration (自動継続)

format_version=3 の `meta.json` は load 時に warning を 1 回出して自動的に
v4 形式へ migrate される。継承される情報:

- `completed_iterations` (= 完了 batch 数として再解釈)
- `schedule`、`init_*_sha256`、`current_params_sha256` 等の v3 既存フィールド

CLI で再指定が必要な情報:

- `--total-pairs` (新規 run での目標値)
- `--batch-pairs` (新規 run の batch 粒度)

`completed_pairs` は `completed_iterations × batch_pairs` で再構築される。
これは「v3 で 1 iter = 1 update = k+1 だった」前提。v3 で
`games_per_iteration = 2 × batch_pairs` として運用していれば SPSA の k 軸は
等価。

### silent migration の制約 (ユーザ責任)

v3 meta は schema 上 `seeds_count` / `games_per_iteration` を保持しないため、
**migration 側で自動検出できないケース**がある:

- v3 で `--seeds 2,3` のような multi-seed run だった場合
- v3 で奇数 `--games-per-iteration` だった場合 (paired antithetic と本来相容れない)

これらは silent migrate を **通過してしまう**可能性がある。検出した時点で
SPSA の進行は不整合になり、結果が以後の SPRT で発散することがある。
**過去の runbook で multi-seed / 奇数 gpi を使っていた run** は、silent migrate
ではなく以下の手順で fresh start すること:

```bash
# 既存 state.params を canonical 起点として新 run dir で fresh start
mkdir -p runs/spsa_v4_$(date -u +%Y%m%d_%H%M%S)
spsa --run-dir runs/spsa_v4_<tag> \
     --init-from runs/old_v3/state.params \
     --total-pairs 5000 --batch-pairs 8 \
     --seed 12345 ...
```

### hard bail (resume 不可)

以下のケースは load_meta が hard bail する:

- v2 以前の format (v3 で既に hard bail 化済み)
- v4 meta で resume 時に `total_pairs` / `batch_pairs` が CLI 指定値と不一致
  (`--force-schedule` で warning に格下げ可能)

完全に新規 run dir で始め直したい場合:

```bash
# 既存 state.params を canonical 起点として新 run dir で fresh start
mkdir -p runs/spsa_v4_$(date -u +%Y%m%d_%H%M%S)
spsa --run-dir runs/spsa_v4_<tag> \
     --init-from runs/old_v3/state.params \
     --total-pairs 5000 --batch-pairs 8 \
     --seed 12345 ...
```

`--use-existing-state-as-init` で既存 `state.params` をそのまま起点にする経路もある。

## 過去の `runs/spsa/` 履歴の解釈

v3 までの `runs/spsa/<tag>/` 配下の履歴:

- `stats.csv` の旧フォーマットはそのまま保持される (v4 spsa は読み書きしない)
- `stats_aggregate.csv` は v4 では生成されない
- `meta.json` の format_version は 3 のまま (silent migration 経由で resume 時に v4 へ書き換わる)
- `final.params` の int 値が `42` 形式で書かれていた箇所は v4 resume 後に `42.000000`
  形式へ更新される (parse は f64 なので互換)

「`stats.csv` は v3 のまま、`stats_aggregate.csv` は v3 まで」と整理し、
新規 run は v4 形式の new run dir に切り替えるのが安全。

## fishtest との整合性まとめ

| 項目                | fishtest                              | v3 rshogi               | v4 rshogi (本 PR)         |
| ------------------- | ------------------------------------- | ----------------------- | ------------------------- |
| paired antithetic   | あり (`play_pair`)                    | 不完全 (start_pos 別)   | あり (start_pos 共有)     |
| 1 batch = 1 update  | あり (`iter += game_pairs`)           | あり (1 iter = 1 update) | あり (1 batch = 1 update) |
| stochastic rounding | あり (`stochastic_round_seeded`)      | なし                    | あり (`floor(v + U)`)     |
| multi-seed          | なし (run 単位で seed 固定)           | あり (`--seeds`)        | なし (撤去)               |
| RNG stream 分離     | あり (flip / rounding 独立)           | なし                    | あり (salt XOR で派生)    |

棋力検証 (selfplay) は本 PR の範囲外。docs/performance/ 配下に検証結果をまとめる
予定。
