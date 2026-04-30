# SPSA実行コマンド一式

## 前提
- リポジトリルートで実行する。
- `rshogi-usi` と `tools/spsa` をローカルビルド可能であること。

## 1. 初回ビルド

```bash
cargo build --release -p rshogi-usi
cargo build --release -p tools --bin generate_spsa_params --bin spsa --bin spsa_stats_to_plot_csv
```

## 2. canonical `.params` の準備

`--init-from` に渡す canonical (起点パラメータファイル) を用意する。
渡せる形式は以下のいずれか:

- **rshogi デフォルト値**: `generate_spsa_params` で生成 (rshogi の `SearchTuneParams::option_specs()` ベース)
- **rshogi 形式の既存 .params**: 過去のチューニング結果や手作業で調整した値
- **YaneuraOu 形式の既存 .params**: YO の `tune.py` 系ツールで生成された
  YO 命名の .params (例: suisho 系の suisho*.params)。YO 駆動時は §10.6 の
  ケース A、rshogi 駆動時は §10.6 のケース B / `yo_to_rshogi_params` 経由

rshogi デフォルト値から始める場合の生成コマンド:

```bash
cargo run --release -p tools --bin generate_spsa_params -- \
  --output spsa_params/canonical.params
```

## 3. SPSA実行（更新）

```bash
RUN_DIR="runs/spsa/$(date -u +%Y%m%d_%H%M%S)"

cargo run --release -p tools --bin spsa -- \
  --run-dir "${RUN_DIR}" \
  --init-from spsa_params/canonical.params \
  --iterations 200 \
  --games-per-iteration 64 \
  --concurrency 8 \
  --startpos-file /path/to/openings.txt \
  --seeds 1,2,3,4 \
  --active-only-regex '^SPSA_(LMR|FUTILITY|NMP)_' \
  --threads 1 \
  --hash-mb 256 \
  --byoyomi 1000 \
  --max-moves 320 \
  --timeout-margin-ms 1000 \
  --early-stop-avg-abs-update-threshold 0.02 \
  --early-stop-result-variance-threshold 0.002 \
  --early-stop-patience 5
```

`<run-dir>` には以下が自動生成される:

| ファイル | 内容 |
|---|---|
| `state.params` | SPSA の live 状態 (反復ごとに上書き) |
| `meta.json` | resume 用メタデータ |
| `values.csv` | 各 iter のパラメータ値履歴 |
| `stats.csv` | per-seed 統計 |
| `stats_aggregate.csv` | seed 横断集計 (seeds が複数のときのみ) |

別パスに置きたい場合は `--meta-file` / `--stats-csv` / `--stats-aggregate-csv`
/ `--param-values-csv` で個別 override 可能。生成を止めたい場合は対応する
`--no-*` フラグを使う。

### 3.1 開始局面ファイル (`--startpos-file`)

1 行 1 局面の USI `position` 形式テキストを渡す。例:

```
startpos moves 2g2f 8c8d 6i7h 8d8e 2f2e 4a3b 3i3h 7a7b ...
startpos moves 2g2f 8c8d 2f2e 8d8e 7g7f 4a3b 8h7g 3c3d ...
sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1 moves 7g7f
```

各反復で `--seeds` ベースに開始局面が抽選される。SPSA はノイズに敏感なので、
**多様で偏りのない局面集** を使うこと（特定戦型のみだと評価が歪む）。

入手先の例:

- やねうら王公式の互角局面集: <https://github.com/yaneurao/YaneuraOu/releases/tag/BalancedPositions2025>
- 自前生成: `selfplay/tournament` や floodgate 棋譜から先頭 N 手を切り出す

`--startpos-file` を省略すると `position startpos`（平手初期局面）から固定で
始まり、SPSA の評価ノイズが激増するため**実用ではほぼ常に指定が必要**。

### 3.2 時間制御の選択

- `--byoyomi <ms>` (既定 1000ms): 1 手あたりの秒読み時間制御。最も単純で SPSA 向き
- `--btime <ms> --binc <ms>`: Fischer モード（持ち時間 + 加算）。長い対局の挙動を測りたい時のみ
- `--nodes <N>`: ノード数固定。CPU 性能差に依存しない厳密比較が必要な場合のみ
  （SPSA はノイズ低減のため通常は時間ベースで十分）

```bash
# Fischer 例: 持ち時間 30s、加算 1s
--btime 30000 --binc 1000
```

`--byoyomi` 使用時は対局相手にも同じ時間ハンデが適用されるよう、SPSA ツールは
内部で MinimumThinkingTime / NetworkDelay 等を**自動調整しない**（外部エンジンに
触らない設計）。フェアな対局のためには §10.6 のように `--usi-option` で明示する。

### 3.3 イテレーション数 / 対局数 / 並列度の目安

経験則ベース。具体値はチューニング対象数とハードウェアで調整:

| 設定 | 目安 | 効くもの |
|---|---|---|
| `--iterations` | 100〜500 (グループ別) / 1000+ (全体) | 収束確度。少なすぎると勾配推定がノイズに埋もれる |
| `--games-per-iteration` | 8〜64 (偶数) | 1 イテレーションあたりの勾配推定精度。Fishtest 流は 4〜32 |
| `--concurrency` | CPU 物理コア数 / `threads` 値 | 壁時計時間。`threads=1` ならコア数いっぱいまで |
| `--seeds` | 3〜5 個 | seed 横断分散の低減（複数 seed の集計で SPSA ノイズの偏りを検出） |
| `--parallel-seeds` | 多コア環境 (≥64T) で ON | iter 内の seed 群を並列実行。`--concurrency` 上限が `games_per_iteration` で頭打ちになるのを解消 |

総対局数 = iterations × games_per_iteration × seeds。
合計が**少なすぎると勾配が信頼できず**、多すぎると壁時計時間が伸びる。
LMR/futility/NMP 等の枝刈り系を 30 個以下のグループでチューニングするなら
`200 × 64 × 4 = 51,200 局` 程度が目安。

### `--parallel-seeds` 利用時の concurrency 調整

`--parallel-seeds` ON 時は `--concurrency` を seed 数で割って各 seed に配分する。
さらに 1 seed あたりの worker 数は `games_per_iteration` で頭打ち（worker 1 つが
1 game を担当するため）。**実効並列度を最大化するには**:

```
--concurrency = seeds × games_per_iteration   （理想）
```

を満たす設定にする。これを満たさない場合、起動時に以下のような warning が出て
未使用 CPU の存在を知らせる:

```
warning: --parallel-seeds の実効並列度が --concurrency より低い
 (concurrency=200, seeds=4, games_per_iteration=64 → per_seed=50 (clamped to 50),
 実効合計=200, 未使用=0)
```

**典型的な悪い例と直し方**:

| 悪い設定 | 何が起きるか | 直す |
|---|---|---|
| `--concurrency 100 --seeds 1,2,3,4 --games-per-iteration 64` | `100 / 4 = 25` worker/seed → 合計 100 で OK だが 64 まで盛れる余地あり | `--concurrency 256` に増やす |
| `--concurrency 384 --seeds 1,2 --games-per-iteration 64` | `384 / 2 = 192` だが `min(192, 64) = 64` で clamp → 256 worker しか動かず 128 未使用 | seed を増やす (`--seeds 1,2,3,4,5,6`) か `--concurrency 128` に減らす |
| `--concurrency 10 --seeds 1,2,3,4` | `10 / 4 = 2` worker/seed → 合計 8、2 worker 未使用 | `--concurrency 8` (倍数化) |

vast.ai 等の多コアインスタンスでは `seeds × games_per_iteration` を先に決めてから
`--concurrency` をその値に合わせるのが事故が少ない。`--parallel-seeds` を付けず
`--concurrency` を盛るのは、`games_per_iteration` 上限で頭打ちになるので非推奨。

## 4. 再開実行（resume）

```bash
cargo run --release -p tools --bin spsa -- \
  --run-dir "${RUN_DIR}" \
  --init-from spsa_params/canonical.params \
  --resume \
  --iterations 100 \
  --games-per-iteration 64 \
  --concurrency 8 \
  --startpos-file /path/to/openings.txt \
  --seeds 1,2,3,4 \
  --threads 1 \
  --hash-mb 256 \
  --byoyomi 1000 \
  --max-moves 320 \
  --timeout-margin-ms 1000
```

`--init-from` を resume 時にも指定すると、`<run-dir>/state.params` の値が canonical
からどれだけ動いたかの diagnostic が起動時に出る (`--strict-init-check` で error
化可能)。スケジュール設定を変更して再開する場合だけ `--force-schedule` を付与する。

### 4.1 `--run-dir` / `--init-from` / `--resume` / `--force-init` の関係

#### `--run-dir` 配下のファイル構成

`--run-dir <dir>` で指定したディレクトリに以下が配置される:

| ファイル | 役割 | override フラグ |
|---|---|---|
| `<run-dir>/state.params` | SPSA の live 状態 (反復ごとに上書き) | (なし) |
| `<run-dir>/meta.json` | resume 用メタデータ | `--meta-file` |
| `<run-dir>/values.csv` | 各 iter のパラメータ値履歴 | `--param-values-csv` |
| `<run-dir>/stats.csv` | per-seed 統計 | `--stats-csv` |
| `<run-dir>/stats_aggregate.csv` | seed 横断集計 | `--stats-aggregate-csv` |

通常運用では override は不要。`runs/spsa/<timestamp>_<tag>/` 形式の dir を
毎回新規に切るのが推奨。

#### 状態遷移マトリクス

`<run-dir>/state.params` の存在有無 × `--init-from` × `--resume` × `--force-init`
の 4 軸を `decide_init_action` で一意に分類する:

| state.params | `--init-from` | `--resume` | `--force-init` | 結果 |
|---|---|---|---|---|
| 不在 | 指定 | - | - | canonical を `state.params` に copy して fresh start |
| 不在 | 指定 | - | ✓ | **bail** (force-init は既存対象が必要) |
| 不在 | 指定 | ✓ | - | **bail** (resume は既存 state が必須) |
| 不在 | 未指定 | - | - | **bail** (入力なし) |
| 不在 | 未指定 | ✓ | - | **bail** (resume は既存 state が必須) |
| 存在 | 指定 | - | - | **bail** (`--resume` か `--force-init` の明示が必要) |
| 存在 | 指定 | ✓ | - | resume + 整合性 diagnostic 出力 |
| 存在 | 指定 | - | ✓ | atomic 上書き (meta/CSV 削除 → state replace) |
| 存在 | 未指定 | - | - | 既存 state で fresh start |
| 存在 | 未指定 | ✓ | - | 通常 resume (meta hash 検証) |
| - | - | ✓ | ✓ | **bail** (resume と force-init は意味が矛盾) |
| - | 未指定 | - | ✓ | **bail** (force-init は init-from が必要) |

`-` はワイルドカード。表は同値クラスで集約 (12 行) しているが、4 軸 16 通り
すべてが一意に分類される (完全網羅性は単体テスト
`decide_covers_all_sixteen_combinations` で担保)。

#### 運用パターン

```bash
RUN_DIR="runs/spsa/$(date -u +%Y%m%d_%H%M%S)"
CANONICAL=spsa_params/canonical.params

# 1 回目: 新規開始 (run-dir 自動作成 + canonical を state.params に copy)
spsa --run-dir "${RUN_DIR}" --init-from "${CANONICAL}" ...

# 続き: resume (canonical との整合性 diagnostic 出力)
spsa --run-dir "${RUN_DIR}" --init-from "${CANONICAL}" --resume ...

# 同 run-dir を破棄して canonical から作り直す
spsa --run-dir "${RUN_DIR}" --init-from "${CANONICAL}" --force-init ...
```

#### 関連フラグ

- **`--force-init`**: `<run-dir>/state.params` を atomic 上書きして再初期化。
  run-dir 直下の `meta.json` / `stats.csv` / `stats_aggregate.csv` / `values.csv`
  も削除する (override で run-dir 外を指定した場合、その override 先は削除対象外)。
  `--resume` と排他。順序保証: meta 削除 (失敗で bail) → 関連 CSV 削除 →
  state.params atomic copy。
- **`--strict-init-check`**: `--resume` + `--init-from` 併用時、整合性が
  median ≥ 0.5σ または max ≥ 5σ を超えたら bail (デフォルトは warn のみ)。
  CI 等で「想定外の resume」を早期検知したい場合に使う。

#### 起動時の startup summary

SPSA 起動直後に `=== SPSA Startup Summary ===` ブロックが stderr に出力される。
init mode、`state.params` / `--init-from` の sha256、active param 数、上位 5
件の値などを表示するので、**起動 5 秒で「想定どおりの値・モードで始まったか」**
を目視確認できる。

#### stdout / stderr の分離

SPSA の進行ログ (per-game progress、iter end summary、early-stop trigger 等) は
すべて stderr に出力される。stdout は CSV writer (ファイル出力) と将来の構造化
出力に予約されている。stderr もログに残したいときは `2>&1 | tee log.txt`。

#### iter 0 スナップショット

fresh start / force-init / use-existing-fresh 時、`<run-dir>/values.csv` の先頭
に `0, <初期値>, ...` 行が記録される。resume 時は append のため重複しない。
これにより「初期値が何であったか」を CSV だけで完全に追える。

## 5. 可視化用CSV変換（任意）

```bash
cargo run --release -p tools --bin spsa_stats_to_plot_csv -- \
  "${RUN_DIR}/stats.csv" \
  --output-csv "${RUN_DIR}/stats.plot.csv" \
  --window 16

cargo run --release -p tools --bin spsa_stats_to_plot_csv -- \
  "${RUN_DIR}/stats_aggregate.csv" \
  --output-csv "${RUN_DIR}/stats_aggregate.plot.csv" \
  --window 16
```

## 6. パラメータの絞り方

一度に全パラメータを動かすと勾配推定のノイズが大きくなり収束が遅い。
10〜20個程度ずつグループ別に回すのが実用的。

### 方法1: `--active-only-regex`（推奨）

`.params` ファイルを変更せず、実行時に対象を絞る。

マッチしないパラメータは摂動されず `.params` ファイルの現在値で固定される。
前回チューニング済みの値をベースに別グループをチューニングする段階的ワークフローに対応。

指定可能な全パターン一覧:

| regex | 対象グループ | 個数 | 説明 |
|---|---|---|---|
| `'^SPSA_LMR_'` | LMR | 25 | Late Move Reductions（テーブル係数、Step16 調整等） |
| `'^SPSA_SINGULAR_'` | Singular Extension | 21 | SE 深さ・beta margin・double/triple extension |
| `'^SPSA_PRIOR_'` | Prior Countermove | 16 | prior quiet/capture countermove bonus |
| `'^SPSA_S14_'` | Step 14 枝刈り | 12 | capture futility, quiet futility, SEE, history pruning |
| `'^SPSA_CONT_HIST'` | Continuation History | 9 | cont history 初期値 + bonus weight (ply 1〜6) |
| `'^SPSA_CORR'` | Correction | 8 | correction value weight + history 更新係数 |
| `'^SPSA_FAIL_HIGH_'` | Fail-High History | 8 | fail-high continuation bonus weight |
| `'^SPSA_NMP_'` | Null Move Pruning | 8 | NMP margin, reduction, verification depth |
| `'^SPSA_FUTILITY_'` | Futility Pruning | 5 | futility margin, improving/opp_worsening 調整 |
| `'^SPSA_EVAL_DIFF_'` | evalDiff | 5 | static eval 差分の clamp, offset, history 係数 |
| `'^SPSA_UPDATE_ALL_'` | History 更新スケール | 5 | quiet/capture bonus/malus, early refute |
| `'^SPSA_STAT_BONUS_'` | Stat Bonus | 4 | stat bonus depth 係数, 上限, TT bonus |
| `'^SPSA_STAT_MALUS_'` | Stat Malus | 4 | stat malus depth 係数, 上限, move count |
| `'^SPSA_PROBCUT_'` | ProbCut | 4 | ProbCut beta margin, depth, dynamic 除算 |
| `'^SPSA_IIR_'` | IIR | 4 | Internal Iterative Reductions 閾値 |
| `'^SPSA_S18_'` | Step 18 Full Depth | 3 | full depth 判定閾値 |
| `'^SPSA_PAWN_HIST'` | Pawn History | 3 | pawn history 初期値 + 正負乗算 |
| `'^SPSA_LOW_PLY_HIST'` | Low Ply History | 3 | low ply history 初期値 + 乗算 + offset |
| `'^SPSA_RAZORING_'` | Razoring | 2 | razoring 閾値 |
| `'^SPSA_ASP_'` | Aspiration Window | 2 | aspiration delta, mean squared 除算 |
| `'^SPSA_TT_MOVE_'` | TT Move | 2 | TT move bonus/malus |
| `'^SPSA_DRAW_'` | Draw Jitter | 2 | 引き分けスコア揺らぎ |
| `'^SPSA_QS_'` | QSearch | 1 | 静止探索 futility |
| `'^SPSA_TT_CUTOFF_'` | TT Cutoff | 4 | TT cutoff cont history penalty + quiet bonus |
| `'^SPSA_SMALL_PROBCUT_'` | Small ProbCut | 1 | small ProbCut beta margin |
| `'^SPSA_MAIN_HIST'` | Main History Init | 1 | main history 初期値 |
| `'^SPSA_CAPTURE_HIST'` | Capture History Init | 1 | capture history 初期値 |

複合パターンの例:

```bash
# 枝刈り系をまとめて（39個）
--active-only-regex '^SPSA_(LMR|FUTILITY|NMP|RAZORING)_'

# correction + history 初期値（13個）
--active-only-regex '^SPSA_(CORR|MAIN_HIST|CAPTURE_HIST|CONT_HIST_INIT|PAWN_HIST_INIT|LOW_PLY_HIST_INIT)'

# Step14 + Step18（15個）
--active-only-regex '^SPSA_S1[48]_'

# history 更新系全般（30個）
--active-only-regex '^SPSA_(STAT_|CONT_HISTORY_|LOW_PLY_HISTORY_|PAWN_HISTORY_|FAIL_HIGH_|UPDATE_ALL_)'

# aspiration + TT + evalDiff（8個）
--active-only-regex '^SPSA_(ASP_|TT_CUTOFF_|EVAL_DIFF_)'
```

### 方法2: `.params` ファイルに `[[NOT USED]]` マーカー

永続的に除外したいパラメータに `[[NOT USED]]` を付加する。

```
SPSA_CORR_PCV_WEIGHT,int,9536,0,32768,163,1638.4
SPSA_LMR_TABLE_COEFF,int,2809,1024,8192,35,358.4 [[NOT USED]]
```

### 推奨チューニング順序

影響が大きいグループから段階的に進める。

1. **LMR / futility / NMP**: 探索木の形を大きく変える枝刈りパラメータ
2. **correction / history init**: 評価補正と履歴テーブルの初期値
3. **Step14 / aspiration / TT cutoff**: 細かい枝刈りとウィンドウ制御

各グループのチューニング結果を `.params` に反映してから次のグループへ進む。

### パラメータ一覧（グループ別・推奨チューニング順）

> **default 値の正本は `crates/rshogi-core/src/search/tune_params.rs`**
> 以下の表は実装に追従するよう手動メンテしているが、乖離があれば実装側を信用すること。
> 一部の符号が YO 源コードと反転しているのは、rshogi 式が `+` に統一して負号をパラメータ値に内包しているため（例: `SPSA_NMP_MARGIN_OFFSET` は `margin = mult*depth + offset` の形なので負値）。

#### 優先度1: LMR（Late Move Reductions）

探索木の形を最も大きく変える。`--active-only-regex '^SPSA_LMR_'`

| regex | USI名 | default | 説明 |
|---|---|---|---|
| LMR | `SPSA_LMR_TABLE_COEFF` | 2809 | reductions テーブル係数 (ln スケール) |
| LMR | `SPSA_LMR_DELTA_SCALE` | 757 | delta による reduction 調整 |
| LMR | `SPSA_LMR_NON_IMPROVING_MULT` | 218 | non-improving 時の追加 reduction |
| LMR | `SPSA_LMR_NON_IMPROVING_DIV` | 512 | non-improving 除算 |
| LMR | `SPSA_LMR_BASE_OFFSET` | 1200 | reduction ベースオフセット |
| LMR | `SPSA_LMR_TTPV_ADD` | 946 | TT-PV ノード補正 |
| LMR | `SPSA_LMR_STEP16_BASE_ADD` | 843 | Step16 ベース加算 |
| LMR | `SPSA_LMR_STEP16_MOVE_COUNT_MUL` | 66 | 手番号による加算 |
| LMR | `SPSA_LMR_STEP16_CORRECTION_DIV` | 30450 | correction value 除算 |
| LMR | `SPSA_LMR_STEP16_CUT_NODE_ADD` | 3094 | cut node 加算 |
| LMR | `SPSA_LMR_STEP16_CUT_NODE_NO_TT_ADD` | 1056 | cut node (TT miss) 追加 |
| LMR | `SPSA_LMR_STEP16_TT_CAPTURE_ADD` | 1415 | TT capture 加算 |
| LMR | `SPSA_LMR_STEP16_CUTOFF_COUNT_ADD` | 1051 | cutoff count 加算 |
| LMR | `SPSA_LMR_STEP16_CUTOFF_COUNT_ALL_NODE_ADD` | 814 | all-node cutoff count 加算 |
| LMR | `SPSA_LMR_STEP16_TTPV_SUB_BASE` | 2618 | TT-PV 減算ベース |
| LMR | `SPSA_LMR_STEP16_TTPV_SUB_PV_NODE` | 991 | PV node 減算 |
| LMR | `SPSA_LMR_STEP16_TTPV_SUB_TT_VALUE` | 903 | TT value 減算 |
| LMR | `SPSA_LMR_STEP16_TTPV_SUB_TT_DEPTH` | 978 | TT depth 減算 |
| LMR | `SPSA_LMR_STEP16_TTPV_SUB_CUT_NODE` | 1051 | cut node 減算 |
| LMR | `SPSA_LMR_STEP16_TT_MOVE_PENALTY` | 2018 | TT move penalty |
| LMR | `SPSA_LMR_STEP16_CAPTURE_STAT_SCALE_NUM` | 803 | capture stat スケール |
| LMR | `SPSA_LMR_STEP16_STAT_SCORE_SCALE_NUM` | 794 | stat score スケール |
| LMR | `SPSA_LMR_RESEARCH_DEEPER_BASE` | 43 | re-search deeper 基準 |
| LMR | `SPSA_LMR_RESEARCH_DEEPER_DEPTH_MUL` | 2 | re-search deeper depth 係数 |
| LMR | `SPSA_LMR_RESEARCH_SHALLOWER_THRESHOLD` | 9 | re-search shallower 閾値 |

#### 優先度1: Futility / Razoring / NMP

枝刈りの閾値。`--active-only-regex '^SPSA_(FUTILITY|RAZORING|NMP)_'`

| regex | USI名 | default | 説明 |
|---|---|---|---|
| FUTILITY | `SPSA_FUTILITY_MARGIN_BASE` | 91 | futility margin 基準値 |
| FUTILITY | `SPSA_FUTILITY_MARGIN_TT_BONUS` | 21 | TT ヒット時のボーナス |
| FUTILITY | `SPSA_FUTILITY_IMPROVING_SCALE` | 2094 | improving 時の調整 (`/1024` 適用) |
| FUTILITY | `SPSA_FUTILITY_OPP_WORSENING_SCALE` | 1324 | 相手悪化時の調整 (`/4096` 適用) |
| FUTILITY | `SPSA_FUTILITY_CORRECTION_DIV` | 158105 | correction value 除算 |
| RAZORING | `SPSA_RAZORING_BASE` | 514 | razoring 閾値ベース |
| RAZORING | `SPSA_RAZORING_DEPTH2` | 294 | razoring depth=2 追加 |
| NMP | `SPSA_NMP_MARGIN_DEPTH_MULT` | 18 | NMP margin depth 係数 |
| NMP | `SPSA_NMP_MARGIN_OFFSET` | -390 | NMP margin オフセット (式: `mult*depth + offset`) |
| NMP | `SPSA_NMP_MARGIN_IMPROVING_BONUS` | 50 | improving 時の追加 |
| NMP | `SPSA_NMP_REDUCTION_BASE` | 7 | NMP reduction ベース |
| NMP | `SPSA_NMP_REDUCTION_DEPTH_DIV` | 3 | NMP reduction depth 除算 |
| NMP | `SPSA_NMP_VERIFICATION_DEPTH` | 16 | NMP verification depth |
| NMP | `SPSA_NMP_MIN_PLY_NUM` | 3 | NMP 最小 ply 分子 |
| NMP | `SPSA_NMP_MIN_PLY_DEN` | 4 | NMP 最小 ply 分母 |

#### 優先度2: Correction / History 初期値

評価補正と履歴テーブル。`--active-only-regex '^SPSA_CORR'`

| regex | USI名 | default | 説明 |
|---|---|---|---|
| CORR | `SPSA_CORR_PCV_WEIGHT` | 9536 | pawn correction weight |
| CORR | `SPSA_CORR_MICV_WEIGHT` | 8494 | minor piece correction weight |
| CORR | `SPSA_CORR_NONPAWN_WEIGHT` | 10132 | non-pawn correction weight |
| CORR | `SPSA_CORR_CNT_WEIGHT` | 7156 | continuation correction weight |
| CORR | `SPSA_CORR_HIST_NONPAWN` | 165 | correction history non-pawn 更新係数 |
| CORR | `SPSA_CORR_HIST_MINOR` | 156 | correction history minor piece 更新係数 |
| CORR | `SPSA_CORR_HIST_CONT_SS2` | 137 | correction history cont (ss-2) 更新係数 |
| CORR | `SPSA_CORR_HIST_CONT_SS4` | 64 | correction history cont (ss-4) 更新係数 |

History 初期値。`--active-only-regex '^SPSA_(MAIN|CAPTURE|CONT|PAWN|LOW_PLY)_HIST_INIT'`

| regex | USI名 | default | 説明 |
|---|---|---|---|
| HIST_INIT | `SPSA_MAIN_HIST_INIT` | 68 | main history 初期値 |
| HIST_INIT | `SPSA_CAPTURE_HIST_INIT` | -689 | capture history 初期値 |
| HIST_INIT | `SPSA_CONT_HIST_INIT` | -529 | continuation history 初期値 |
| HIST_INIT | `SPSA_PAWN_HIST_INIT` | -1238 | pawn history 初期値 |
| HIST_INIT | `SPSA_LOW_PLY_HIST_INIT` | 97 | low ply history 初期値 |

#### 優先度2: Step 14 枝刈り

手単位の枝刈り判定。`--active-only-regex '^SPSA_S14_'`

| regex | USI名 | default | 説明 |
|---|---|---|---|
| S14 | `SPSA_S14_FUT_BASE` | 231 | capture futility ベース |
| S14 | `SPSA_S14_FUT_LMR_MULT` | 211 | capture futility LMR depth 係数 |
| S14 | `SPSA_S14_FUT_CAPT_HIST` | 130 | capture futility history スケール |
| S14 | `SPSA_S14_CONT_HIST_THRESH` | -4312 | continuation history 枝刈り閾値 |
| S14 | `SPSA_S14_MAIN_HIST_NUM` | 76 | main history 加算分子 |
| S14 | `SPSA_S14_MAIN_HIST_DEN` | 32 | main history 加算分母 |
| S14 | `SPSA_S14_LMR_HIST_DIV` | 3220 | LMR depth history 除算 |
| S14 | `SPSA_S14_QFUT_BASE` | 47 | quiet futility ベース |
| S14 | `SPSA_S14_QFUT_NO_BEST` | 171 | quiet futility (best move なし) 加算 |
| S14 | `SPSA_S14_QFUT_LMR_MULT` | 134 | quiet futility LMR depth 係数 |
| S14 | `SPSA_S14_QFUT_EVAL_ALPHA` | 90 | quiet futility eval > alpha 加算 |
| S14 | `SPSA_S14_SEE_MULT` | -27 | SEE 枝刈り閾値係数 |

#### 優先度3: Singular Extension

`--active-only-regex '^SPSA_SINGULAR_'`

rshogi 式は `+` に統一しているため、YO 式の `-X *` は rshogi 側で負値パラメータとして格納される（例: `_NON_TT_CAPTURE = -212`）。

| regex | USI名 | default | 説明 |
|---|---|---|---|
| SINGULAR | `SPSA_SINGULAR_MIN_DEPTH_BASE` | 6 | SE 最小 depth ベース |
| SINGULAR | `SPSA_SINGULAR_MIN_DEPTH_TT_PV_ADD` | 1 | TT-PV 時の追加 |
| SINGULAR | `SPSA_SINGULAR_TT_DEPTH_MARGIN` | 3 | TT depth マージン |
| SINGULAR | `SPSA_SINGULAR_BETA_MARGIN_BASE` | 56 | beta margin ベース |
| SINGULAR | `SPSA_SINGULAR_BETA_MARGIN_TT_PV_NON_PV_ADD` | 81 | TT-PV かつ non-PV 時の追加 |
| SINGULAR | `SPSA_SINGULAR_BETA_MARGIN_DIV` | 60 | beta margin depth 除算 |
| SINGULAR | `SPSA_SINGULAR_DEPTH_DIV` | 2 | SE search depth 除算 |
| SINGULAR | `SPSA_SINGULAR_DOUBLE_MARGIN_BASE` | -4 | double ext margin ベース |
| SINGULAR | `SPSA_SINGULAR_DOUBLE_MARGIN_PV_NODE` | 198 | PV node 追加 |
| SINGULAR | `SPSA_SINGULAR_DOUBLE_MARGIN_NON_TT_CAPTURE` | -212 | non-TT capture 追加 (YO `-212 *`) |
| SINGULAR | `SPSA_SINGULAR_CORR_VAL_ADJ_DIV` | 229958 | correction value 調整除算 |
| SINGULAR | `SPSA_SINGULAR_DOUBLE_MARGIN_TT_MOVE_HIST_MULT` | -921 | TT move history 係数 (YO `-921 *`) |
| SINGULAR | `SPSA_SINGULAR_DOUBLE_MARGIN_TT_MOVE_HIST_DIV` | 127649 | TT move history 除算 |
| SINGULAR | `SPSA_SINGULAR_DOUBLE_MARGIN_LATE_PLY_PENALTY` | 45 | late ply ペナルティ |
| SINGULAR | `SPSA_SINGULAR_TRIPLE_MARGIN_BASE` | 76 | triple ext margin ベース |
| SINGULAR | `SPSA_SINGULAR_TRIPLE_MARGIN_PV_NODE` | 308 | PV node 追加 |
| SINGULAR | `SPSA_SINGULAR_TRIPLE_MARGIN_NON_TT_CAPTURE` | -250 | non-TT capture 追加 (YO `-250 *`) |
| SINGULAR | `SPSA_SINGULAR_TRIPLE_MARGIN_TT_PV` | 92 | TT-PV 追加 |
| SINGULAR | `SPSA_SINGULAR_TRIPLE_MARGIN_LATE_PLY_PENALTY` | 52 | late ply ペナルティ |
| SINGULAR | `SPSA_SINGULAR_NEGATIVE_EXTENSION_TT_FAIL_HIGH` | -3 | TT fail-high 時の負延長 |
| SINGULAR | `SPSA_SINGULAR_NEGATIVE_EXTENSION_CUT_NODE` | -2 | cut node 時の負延長 |

#### 優先度3: Aspiration Window / Step 18 / TT cutoff / evalDiff

`--active-only-regex '^SPSA_(ASP|S18|TT_CUTOFF|EVAL_DIFF|SMALL_PROBCUT)_'`

| regex | USI名 | default | 説明 |
|---|---|---|---|
| ASP | `SPSA_ASP_DELTA_BASE` | 5 | aspiration delta 初期値 |
| ASP | `SPSA_ASP_MEAN_SQ_DIV` | 9000 | mean squared score 除算 |
| S18 | `SPSA_S18_NO_TT_ADD` | 1118 | TT miss 時の full depth 加算 |
| S18 | `SPSA_S18_R_THRESH1` | 3212 | full depth 閾値 1 |
| S18 | `SPSA_S18_R_THRESH2` | 4784 | full depth 閾値 2 |
| TT_CUTOFF | `SPSA_TT_CUTOFF_CONT_PENALTY` | -2142 | TT cutoff cont history penalty |
| TT_CUTOFF | `SPSA_TT_CUTOFF_QUIET_BONUS_DEPTH_MULT` | 130 | TT cutoff quiet bonus depth 係数 |
| TT_CUTOFF | `SPSA_TT_CUTOFF_QUIET_BONUS_OFFSET` | -71 | TT cutoff quiet bonus オフセット |
| TT_CUTOFF | `SPSA_TT_CUTOFF_QUIET_BONUS_MAX` | 1043 | TT cutoff quiet bonus 上限 |
| SMALL_PROBCUT | `SPSA_SMALL_PROBCUT_BETA_MARGIN` | 418 | small ProbCut beta margin |
| EVAL_DIFF | `SPSA_EVAL_DIFF_CLAMP_MIN` | -200 | evalDiff clamp 下限 |
| EVAL_DIFF | `SPSA_EVAL_DIFF_CLAMP_MAX` | 156 | evalDiff clamp 上限 |
| EVAL_DIFF | `SPSA_EVAL_DIFF_OFFSET` | 58 | evalDiff オフセット |
| EVAL_DIFF | `SPSA_EVAL_DIFF_MAIN_HIST` | 9 | evalDiff main history 係数 |
| EVAL_DIFF | `SPSA_EVAL_DIFF_PAWN_HIST` | 13 | evalDiff pawn history 係数 |

#### 優先度3: History 更新 / ProbCut / QSearch / その他

`--active-only-regex '^SPSA_(STAT_|PROBCUT|QS_|IIR|DRAW_JITTER)'`

| regex | USI名 | default | 説明 |
|---|---|---|---|
| STAT | `SPSA_STAT_BONUS_DEPTH_MULT` | 121 | stat bonus depth 係数 |
| STAT | `SPSA_STAT_BONUS_OFFSET` | -77 | stat bonus オフセット |
| STAT | `SPSA_STAT_BONUS_MAX` | 1633 | stat bonus 上限 |
| STAT | `SPSA_STAT_BONUS_TT_BONUS` | 375 | TT move bonus |
| STAT | `SPSA_STAT_MALUS_DEPTH_MULT` | 825 | stat malus depth 係数 |
| STAT | `SPSA_STAT_MALUS_OFFSET` | -196 | stat malus オフセット |
| STAT | `SPSA_STAT_MALUS_MAX` | 2159 | stat malus 上限 |
| STAT | `SPSA_STAT_MALUS_MOVE_COUNT_MULT` | 16 | stat malus move count 係数 |
| PROBCUT | `SPSA_PROBCUT_BETA_MARGIN` | 224 | ProbCut beta margin |
| PROBCUT | `SPSA_PROBCUT_IMPROVING_SUB` | 64 | improving 時の減算 |
| PROBCUT | `SPSA_PROBCUT_DYNAMIC_DIV` | 306 | 動的 beta 除算 |
| PROBCUT | `SPSA_PROBCUT_DEPTH_BASE` | 5 | ProbCut depth |
| QS | `SPSA_QS_FUTILITY_BASE` | 352 | 静止探索 futility ベース |
| IIR | `SPSA_IIR_SHALLOW` | 1 | IIR 浅い depth 閾値 |
| IIR | `SPSA_IIR_DEEP` | 3 | IIR 深い depth 閾値 |
| IIR | `SPSA_IIR_DEPTH_BOUNDARY` | 10 | IIR depth 境界 |
| IIR | `SPSA_IIR_EVAL_SUM` | 177 | IIR eval sum 閾値 |
| DRAW | `SPSA_DRAW_JITTER_MASK` | 2 | 引き分けスコア揺らぎマスク |
| DRAW | `SPSA_DRAW_JITTER_OFFSET` | -1 | 引き分けスコア揺らぎオフセット |

#### 優先度3: History 詳細（Continuation / Pawn / Prior / TT move）

パラメータ数が多いが個別の影響は小さい。
`--active-only-regex '^SPSA_(CONT_HISTORY|LOW_PLY|PAWN_HISTORY|FAIL_HIGH|UPDATE_ALL|PRIOR|TT_MOVE)_'`

| regex | USI名 | default | 説明 |
|---|---|---|---|
| LOW_PLY | `SPSA_LOW_PLY_HISTORY_MULTIPLIER` | 761 | low ply history 乗算 |
| LOW_PLY | `SPSA_LOW_PLY_HISTORY_OFFSET` | 0 | low ply history オフセット |
| CONT | `SPSA_CONT_HISTORY_MULTIPLIER` | 955 | cont history bonus 乗算 |
| CONT | `SPSA_CONT_HISTORY_NEAR_PLY_OFFSET` | 88 | cont history 近 ply オフセット |
| CONT | `SPSA_CONT_HISTORY_WEIGHT_1`〜`6` | 1157,648,288,576,140,441 | cont history weight (ply 1〜6) |
| FAIL_HIGH | `SPSA_FAIL_HIGH_CONT_BASE_NUM` | 1412 | fail-high cont bonus ベース |
| FAIL_HIGH | `SPSA_FAIL_HIGH_CONT_NEAR_PLY_OFFSET` | 80 | fail-high cont 近 ply オフセット |
| FAIL_HIGH | `SPSA_FAIL_HIGH_CONT_WEIGHT_1`〜`6` | 1108,652,273,572,126,449 | fail-high cont weight (ply 1〜6) |
| PAWN | `SPSA_PAWN_HISTORY_POS_MULTIPLIER` | 850 | pawn history 正方向乗算 |
| PAWN | `SPSA_PAWN_HISTORY_NEG_MULTIPLIER` | 550 | pawn history 負方向乗算 |
| UPDATE | `SPSA_UPDATE_ALL_QUIET_BONUS_SCALE_NUM` | 881 | quiet bonus スケール |
| UPDATE | `SPSA_UPDATE_ALL_QUIET_MALUS_SCALE_NUM` | 1083 | quiet malus スケール |
| UPDATE | `SPSA_UPDATE_ALL_CAPTURE_BONUS_SCALE_NUM` | 1482 | capture bonus スケール |
| UPDATE | `SPSA_UPDATE_ALL_CAPTURE_MALUS_SCALE_NUM` | 1397 | capture malus スケール |
| UPDATE | `SPSA_UPDATE_ALL_EARLY_REFUTE_PENALTY_SCALE_NUM` | 614 | early refute penalty スケール |
| PRIOR | `SPSA_PRIOR_QUIET_CM_BONUS_SCALE_BASE` | -228 | prior quiet CM bonus ベース |
| PRIOR | `SPSA_PRIOR_QUIET_CM_PARENT_STAT_DIV` | 104 | parent stat 除算 |
| PRIOR | `SPSA_PRIOR_QUIET_CM_DEPTH_MUL` | 63 | depth 係数 |
| PRIOR | `SPSA_PRIOR_QUIET_CM_DEPTH_CAP` | 508 | depth 上限 |
| PRIOR | `SPSA_PRIOR_QUIET_CM_MOVE_COUNT_BONUS` | 184 | move count bonus |
| PRIOR | `SPSA_PRIOR_QUIET_CM_EVAL_BONUS` | 143 | eval bonus |
| PRIOR | `SPSA_PRIOR_QUIET_CM_EVAL_MARGIN` | 92 | eval margin |
| PRIOR | `SPSA_PRIOR_QUIET_CM_PARENT_EVAL_BONUS` | 149 | parent eval bonus |
| PRIOR | `SPSA_PRIOR_QUIET_CM_PARENT_EVAL_MARGIN` | 70 | parent eval margin |
| PRIOR | `SPSA_PRIOR_QUIET_CM_SCALED_DEPTH_MUL` | 144 | scaled depth 係数 |
| PRIOR | `SPSA_PRIOR_QUIET_CM_SCALED_OFFSET` | -92 | scaled offset |
| PRIOR | `SPSA_PRIOR_QUIET_CM_SCALED_CAP` | 1365 | scaled 上限 |
| PRIOR | `SPSA_PRIOR_QUIET_CM_CONT_SCALE_NUM` | 400 | cont scale |
| PRIOR | `SPSA_PRIOR_QUIET_CM_MAIN_SCALE_NUM` | 220 | main scale |
| PRIOR | `SPSA_PRIOR_QUIET_CM_PAWN_SCALE_NUM` | 1164 | pawn scale |
| TT_MOVE | `SPSA_TT_MOVE_BONUS` | 811 | TT move bonus |
| TT_MOVE | `SPSA_TT_MOVE_MALUS` | -848 | TT move malus |
| PRIOR | `SPSA_PRIOR_CAPTURE_CM_BONUS` | 964 | prior capture CM bonus |

## 7. `.params` ファイルの step / delta

`generate_spsa_params` が自動算出する値の意味:

| 列 | 意味 | 算出式 |
|---|---|---|
| `step` | 勾配推定時の摂動量（SPSAの c_t に対応） | `max(1, round((max - min) / 200))` |
| `delta` | パラメータ更新の移動量（SPSAの a_t に対応） | `(max - min) / 20` |

- `step` が小さすぎると勾配推定がノイズに埋もれる。大きすぎると近似精度が落ちる。
- `delta` が大きすぎると発散する。小さすぎると収束が遅い。
- 実行時に `--scale` で摂動量を一律スケール、`--mobility` で移動量を一律スケールできる。
- 個別調整が必要な場合は `.params` ファイルを直接編集する。

## 8. よく使う調整項目
- 開始局面ファイルを固定する: `--startpos-file /path/to/openings.txt`
- 開始局面ファイルを必須化する: `--require-startpos-file`
- 単一局面で回す: `--sfen "<sfen>" --random-startpos false`
- 対局を並列化する: `--concurrency <N>`（既定は `1`）
- iter 内の seed 群も並列実行する: `--parallel-seeds`（多コア環境向け、§3 の調整ルール参照）
- 早期停止を有効化する: `--early-stop-avg-abs-update-threshold` / `--early-stop-result-variance-threshold` / `--early-stop-patience`
- 速度優先の暫定実行: `--games-per-iteration` と `--iterations` を小さくする

## 9. 生成物

`--run-dir <dir>` 配下に以下が自動生成される:

| 生成物 | 既定パス | override フラグ | opt-out フラグ |
|---|---|---|---|
| 更新済みパラメータ | `<run-dir>/state.params` (上書き) | — | — |
| 再開メタデータ | `<run-dir>/meta.json` | `--meta-file` | — |
| seed 単位統計 | `<run-dir>/stats.csv` | `--stats-csv` | `--no-stats-csv` |
| seed 集計統計 (複数 seed 時のみ) | `<run-dir>/stats_aggregate.csv` | `--stats-aggregate-csv` | `--no-stats-aggregate-csv` |
| パラメータ履歴 | `<run-dir>/values.csv` | `--param-values-csv` | `--no-param-values-csv` |
| 可視化CSV (`spsa_stats_to_plot_csv` 出力) | — | `--output-csv` | — |

override フラグを使うと、対象ファイルだけ任意のパスに置ける (他は run-dir 既定のまま)。
通常運用では override は不要。

> 注: `--stats-csv` のみ override し `--stats-aggregate-csv` を未指定にすると、
> aggregate の既定パスは `<stats-csv の値>.aggregate.csv` (例: `--stats-csv foo.csv`
> なら `foo.csv.aggregate.csv`) に派生する。両方を override したいなら個別に指定する。

### 9.1 `stats.csv` (seed 単位統計) の主要列

| 列 | 意味 |
|---|---|
| `iteration` | 1-indexed の反復番号 |
| `seed` | 反復で使った乱数 seed |
| `games` | 当該反復の対局数 |
| `plus_wins` / `minus_wins` / `draws` | + 摂動側 / - 摂動側 / 引き分けの局数 |
| `raw_result` | + 側勝率ベースのスコア（+1: + 全勝 / 0: 引き分け / -1: - 全勝） |
| `active_params` | 当該反復で perturb 対象となった active パラメータ数 |
| `avg_abs_shift` | active パラメータの摂動量の平均（c_k に依存） |
| `updated_params` | 当該反復で値が動いた parameter 数 |
| `avg_abs_update` / `max_abs_update` | 値更新量の平均と最大 |
| `total_games` | これまでの累積対局数 |

### 9.2 `stats_aggregate.csv` (seed 横断集計、複数 seed 時のみ)

`raw_result_mean` / `raw_result_variance` / `plus_wins_mean` 等。`variance` が
高い反復は seed 間で結果が揺れているサインで、SPSA の収束判定には
**aggregate 側の `_variance` を見る**のが基本。

### 9.3 `values.csv` (パラメータ履歴、wide 形式)

各列が 1 パラメータ、各行が 1 反復。`spsa_stats_to_plot_csv` で window 平均をかけて
プロットすると個別パラメータの収束軌跡が見やすい。

### 9.4 収束判定の目安

`--early-stop-avg-abs-update-threshold` / `--early-stop-result-variance-threshold`
/ `--early-stop-patience` を指定すると条件成立で SPSA が早期終了する。経験則:

- `avg_abs_update < 0.02` 程度（摂動量比で）が連続 5 反復続けば「ほぼ動いていない」
- `result_variance < 0.002` 程度なら seed 間ばらつきが小さい (= 結果が再現的)

両方の閾値を満たす反復が `--early-stop-patience` 回連続したら停止。値はチューニング
対象の感度で調整 (枝刈り系は感度高、history 初期値は低、等)。

## 10. YaneuraOu ⇔ rshogi `.params` 変換

YO 側のチューニング結果（例: suisho10）を rshogi に取り込むための一連のツール。
正本マッピング表は `tune/yo_rshogi_mapping.toml`（102 エントリ）。

### 10.1 ビルド

```bash
cargo build --release -p tools \
  --bin yo_to_rshogi_params \
  --bin rshogi_to_yo_params \
  --bin check_param_mapping \
  --bin build_param_mapping
```

### 10.2 YO → rshogi 変換

```bash
cargo run --release -p tools --bin yo_to_rshogi_params -- \
  --yo-params tune/suisho10.params \
  --base spsa_params/<rshogi_base>.params \   # min/max/step を引き継ぐベース（省略時はデフォルト range）
  --mapping tune/yo_rshogi_mapping.toml \
  --output spsa_params/from_yo_suisho10.params
```

- マップ可能な行は YO 値（必要なら符号反転）で上書き
- マップ外の rshogi パラメータは `--base` の値（or デフォルト）を維持
- 変換結果が rshogi 側 min/max を超える場合は warning（`--strict-range` で error）

### 10.3 rshogi → YO 変換（逆方向）

```bash
cargo run --release -p tools --bin rshogi_to_yo_params -- \
  --rshogi-params spsa_params/<rshogi_tuned>.params \
  --base tune/suisho10.params \   # min/max/step を引き継ぐベース（省略時は値ベース簡易生成）
  --mapping tune/yo_rshogi_mapping.toml \
  --output tune/from_rshogi.params
```

- rshogi 独自パラメータ（`unmapped.rshogi`）は YO 出力に含まれない（info 出力）
- `--base` の YO 行順序を保持

### 10.4 整合性検証

```bash
cargo run --release -p tools --bin check_param_mapping -- \
  --mapping tune/yo_rshogi_mapping.toml \
  --yo-params tune/suisho10.params \
  --rshogi-params spsa_params/suisho10_converted.params
```

検証項目:
1. マッピング表の YO/rshogi 名重複チェック
2. rshogi 名が `tune_params.rs::SearchTuneParams::option_specs()` に存在するか
3. tune_params に存在するが mapping にも `unmapped.rshogi` にも記載のない rshogi 名（warning）
4. 与えられた YO/rshogi `.params` ペアでマッピングを通したときの値一致

`--strict` 指定時は値不一致 1 件でも exit 1。

#### YO バイナリとの整合性検証

`--yo-binary <path>` を渡すと、YO バイナリを起動して `usi` 応答を受信し、
公開されている USI option 一覧と mapping 表 (`mappings.yo` ∪ `unmapped.yo`)
の整合性を検証する。tune.py 注入が変わって mapping 表が陳腐化したことを
CI で拾う用途。

```bash
cargo run --release -p tools --bin check_param_mapping -- \
  --mapping tune/yo_rshogi_mapping.toml \
  --yo-binary /path/to/YaneuraOu-tune-patched
```

検出される 2 種類のドリフト:

- **YO で公開されているが mapping にも `unmapped.yo` にも記載のない option**:
  tune.py 注入が増えた可能性（標準 USI option なら `unmapped.yo` に追記）
- **mapping/`unmapped.yo` にあるが YO バイナリの USI option に存在しない**:
  旧 mapping の残骸 or 条件付き注入（YO ビルド設定で出る/出ないが変わるもの）

`--strict` 指定時は上記いずれかの不整合があれば exit 1。

CI 例（mapping 表のドリフト検出を制度化）:

```bash
# YO バイナリと mapping 表の整合性を不一致 0 で要求
cargo run --release -p tools --bin check_param_mapping -- \
  --mapping tune/yo_rshogi_mapping.toml \
  --yo-params tune/suisho10.params \
  --rshogi-params spsa_params/suisho10_converted.params \
  --yo-binary /path/to/YaneuraOu-tune-patched \
  --strict
```

worker_clear1 系のような条件付き注入で warn が予期される YO ビルドを CI 対象に
する場合は、それらを `unmapped.yo` に移すか CI を `--strict` なしで走らせて
warn 件数だけを監視する運用にする。

### 10.5 マッピング表の更新フロー

新たな YO ↔ rshogi 対応を追加する／既存対応を見直す場合:

1. `build_param_mapping` で正本ペアから自動マッピング候補を抽出
   ```bash
   cargo run --release -p tools --bin build_param_mapping -- \
     --yo-params tune/suisho10.params \
     --rshogi-params spsa_params/suisho10_converted.params \
     --output /tmp/auto_mapping.toml
   ```
2. `/tmp/auto_mapping.toml` の `[ambiguous]` ブロックを `tune/suisho10.tune` のソース文脈と
   `crates/rshogi-core/src/search/tune_params.rs` の式実装で人手レビューして確定
3. `tune/yo_rshogi_mapping.toml` を編集
4. `check_param_mapping` で整合性確認

### 符号反転について

rshogi の式は `+` に統一されているため、YO 式 `-X *` のような明示的な減算項は、
rshogi 側で**負値パラメータ**として格納される。マッピング表では `sign_flip = true`
で表現する。例:

- `Search_nullmove_2` (YO: `+ 390`) ↔ `SPSA_NMP_MARGIN_OFFSET` (rshogi: `-390`)
- `Search_Extensions3_3` (YO: `- 212 *`) ↔ `SPSA_SINGULAR_DOUBLE_MARGIN_NON_TT_CAPTURE` (rshogi: `-212`)
- `update_all_stats_1c_2` (YO: `- 77`) ↔ `SPSA_STAT_BONUS_OFFSET` (rshogi: `-77`)

### 10.6 rshogi SPSA で YaneuraOu エンジンを駆動する

YO を rshogi の SPSA ループで叩いてチューニングしたい場合、YO 側のチューニング対象
パラメータを **USI option として顕在化させる**前処理がユーザ側で必要になる。
本リポジトリのツールでは肩代わりせず、既存の YaneuraOu-ScriptCollection の流儀に従う。

#### 10.6.0 YO 側の前処理（必須・ユーザ作業）

**参考リポジトリ**: <https://github.com/yaneurao/YaneuraOu-ScriptCollection/tree/main/SPSA>

YO のチューニング対象は素のソースだと `constexpr int param = N;` のような定数なので、
そのままでは USI 経由で値を変えられない。`tune.py` がソースに `TUNE(...)` マクロを
注入することで、Stockfish 由来の汎用機構経由で USI option として現れるようになる。

##### Step 1. tune.py の入手

YaneuraOu-ScriptCollection の `SPSA/` ディレクトリ、もしくは本リポジトリの `tune/` 直下に
`tune.py` と `ParamLib.py` が同梱されている。

##### Step 2. `.tune` ファイルを YO ソースに当てる

`.tune` ファイルはチューニング対象の C++ ソース断片に `@` マーカーを付けたテンプレート。
`tune.py tune` を実行すると、`yaneuraou-search.cpp` 中の `%%TUNE_DECLARATION%%` /
`%%TUNE_OPTIONS%%` プレースホルダ位置に `TUNE(SetRange(min, max), param_name, ...)` の
コードを注入し、対象定数を runtime 可変な変数に置換する。

```bash
# YO リポジトリのルートで:
cd /path/to/YaneuraOu
python3 /path/to/rshogi/tune/tune.py tune \
  /path/to/rshogi/tune/suisho10.tune \
  source/  # YO の C++ ソースディレクトリ
```

実行後、`source/engine/yaneuraou-engine/yaneuraou-search.cpp` などが書き換わる
（git diff で確認可）。元に戻す場合は `git checkout source/`。

##### Step 3. YO をビルド

ビルドコマンドは YO のアーキテクチャ（AVX2/AVX512/NEON 等）・評価関数種別
（HalfKP / HalfKAv2 / NNUE 系）・OS/コンパイラに依存するため、本ドキュメントでは固定の
レシピは示さない。YO の `Makefile` / `README` / ScriptCollection の例を参照のこと。
代表的には:

```bash
cd /path/to/YaneuraOu/source
make YANEURAOU_EDITION=YANEURAOU_ENGINE_NNUE TARGET_CPU=AVX2 -j$(nproc)
# 出来たバイナリ（例: YaneuraOu-by-gcc）を tune-patched 用に取っておく
cp YaneuraOu-by-gcc /path/to/YaneuraOu-tune-patched
```

##### Step 4. USI option として現れていることを確認

```bash
echo "usi" | /path/to/YaneuraOu-tune-patched | grep "option name correction_value_1"
# option name correction_value_1 type spin default 9536 min 0 max 17734
```

これが返れば前処理完了。返らない場合は Step 2 の注入が当たっていないか、Step 3 が
古いバイナリ。

#### 10.6.0' YO 駆動時の `--usi-option` について（重要）

YO バイナリは NNUE 評価関数のロード、進行度ファイル、定跡無効化、フェアな対局のための
時間制御パラメータ等を `setoption` で受け取らないと探索が機能しない or 公平な対局が
できないことがある。**必要な USI option はビルド構成・評価関数・対局相手の構成で
全く異なる**ため本ドキュメントでは固定セットを示さない。

判断材料:

- `echo usi | <YO バイナリ>` で公開されている option 一覧を確認する
- `runs/selfplay/<RUN>/meta.json` に過去の実績設定が残っていればそれを `--usi-option`
  に転記するのが安全（同じバイナリで動作実績がある）
- フェアな対局のためには SPSA ツールが**自動調整しない**以下のような項目に注意:
  - 評価関数指定 (`EvalDir` or `EvalFile`)、関連スカラ (`FV_SCALE` 等)
  - 進行度・定跡関連 (`BookFile=no_book` 等で外部リソース依存を排除)
  - ネットワーク遅延補正、最小思考時間、PvInterval 等の対局公平性パラメータ
- `selfplay/tournament` ツールはこれらを自動設定するが、`spsa` ツールは**ユーザ責任**で
  全部 `--usi-option <Name>=<Value>` 形式で渡す必要がある

#### 10.6.1 ケース A: YO 形式 `.params` で YO を直接チューニング（推奨・最も単純）

`.params` の name 列が YO のビルド済み USI option 名と一致していれば、rshogi の `spsa`
はその name を `setoption` にそのまま流すだけなので、**変換ツール・マッピング表は不要**。

正本 (例: `tune/suisho10.params`) は `--init-from` に指定する。
反復用ファイル (`<run-dir>/state.params`) は spsa が自動生成・上書きするので、
正本そのものが書き換えられることはない。

```bash
RUN_DIR="runs/spsa/$(date -u +%Y%m%d_%H%M%S)_yo_suisho10"

cargo run --release -p tools --bin spsa -- \
  --run-dir "${RUN_DIR}" \
  --init-from tune/suisho10.params \
  --engine-path /path/to/YaneuraOu-tune-patched \
  --iterations 200 --games-per-iteration 64 \
  --concurrency 8 --threads 1 --hash-mb 256 --byoyomi 1000 \
  --startpos-file /path/to/openings.txt \
  --seeds 1,2,3,4
```

`<run-dir>/state.params` が YO 形式のまま運用に乗る (`tune.py apply` の入力に
そのまま使える)。`tune/suisho10.params` (正本) は触られない。
CSV 出力 (`stats.csv` / `stats_aggregate.csv` / `values.csv`) も同 run-dir に
自動生成される (§9 参照)。

##### case A 用の `--active-only-regex` 例 (YO 命名)

YO 命名は `<context>_<n>` のスネークケース ＋ 数字接尾辞。`tune/suisho10.tune` の
`#context` に対応。代表的なグループ:

| regex | 対象 |
|---|---|
| `'^correction_value_'` | 評価補正係数 (4 個) |
| `'^update_correction_history'` | 補正履歴更新係数 |
| `'^Search_razoring_'` | razoring 閾値 (2 個) |
| `'^Search_nullmove_'` | NMP margin (2 個) |
| `'^Search_Probcut_'` | ProbCut margin |
| `'^Search_Decrease_reduction_for_PvNodes_'` | LMR Step16 系（最大グループ） |
| `'^Search_Extensions[0-9]_'` | Singular extension margin |
| `'^Search_futility_value_'` | Step14 capture futility |
| `'^Search_Continuation_history_based_pruning'` | Step14 history-based pruning |
| `'^Search_Full_depth_search_threshold_'` | Step18 full depth threshold |
| `'^conthist_bonuses_'` | Continuation history weight (6 個) |
| `'^update_all_stats_'` | stat bonus / malus |
| `'^update_quiet_histories_'` | quiet history multiplier |
| `'^Search_static_evaluation_'` | evalDiff |
| `'^aspiration_window_'` | aspiration delta |
| `'^YaneuraOuWorker_(reduction\|clear[123])_'` | reduction table / history fill 初期値 |

正確なグループ構成は `tune/suisho10.tune` の `#context` を参照。
複合パターン例: `'^Search_(Decrease_reduction_for_PvNodes|Full_depth_search_threshold)_'`
（LMR Step16 + Step18）。

#### 10.6.2 ケース B: rshogi 形式 `.params` で YO を駆動（`--engine-param-mapping`）

rshogi 側の `.params` フォーマット（`SPSA_*` 命名）のまま YO バイナリを駆動したい場合、
`spsa` バイナリに `--engine-param-mapping` を渡すと、`.params` 内の rshogi 名 (`SPSA_*`)
を `setoption` する直前にエンジン側名前空間（YO 名）に翻訳し、必要なら符号を反転する。

```bash
RUN_DIR="runs/spsa/$(date -u +%Y%m%d_%H%M%S)_yo_via_rshogi"

cargo run --release -p tools --bin spsa -- \
  --run-dir "${RUN_DIR}" \
  --init-from spsa_params/suisho10_converted.params \
  --engine-path /path/to/YaneuraOu-tune-patched \
  --engine-param-mapping tune/yo_rshogi_mapping.toml \
  --iterations 200 --games-per-iteration 64 \
  --concurrency 8 --threads 1 --hash-mb 256 --byoyomi 1000 \
  --startpos-file /path/to/openings.txt \
  --seeds 1,2,3,4
```

- `.params` の `SPSA_*` 名は `setoption` 時に YO 名（`correction_value_1` 等）へ翻訳され、
  `sign_flip = true` の項は値の符号を反転して送出される
- マッピング表にない rshogi 名（rshogi 独自 / `unmapped.rshogi` のもの）は名前をそのまま
  渡すが、YO バイナリ側にその option がないので `set_option_if_available` で黙って無視される
- そのため YO 駆動時は `--active-only-regex` で YO 対応グループに絞ると無駄な `setoption`
  を避けられる
- **range の整合性は運用責任**: SPSA 側の clamp は rshogi 側 `.params` の min/max
  でしか効かない。`sign_flip = true` のパラメータは rshogi 範囲 → 符号反転 → YO に送出
  される過程で YO 側 USI option の min/max からはみ出すケースが理屈上ありうる。
  チューニング前に `check_param_mapping --yo-binary <YOバイナリ> --yo-params
  tune/<YO形式>.params --rshogi-params spsa_params/<rshogi形式>.params`
  で双方の range 整合性を確認しておくこと。YO 側で range 外の値は USI option として
  受理されない可能性がある。

#### 10.6.3 チューニング結果を YO 本体に焼き込む（`tune.py apply`）

SPSA ループで得られた最終値を YO の production バイナリに反映するには、`tune.py apply`
でソースに焼き戻し → production ビルドが必要（YO の TUNE() マクロは production リリース
時には残したくないため、定数として書き戻す）。

```bash
# ケース A の場合: state.params が YO 形式のままなので直接 apply
cd /path/to/YaneuraOu
python3 /path/to/rshogi/tune/tune.py apply \
  /path/to/rshogi/runs/spsa/<RUN>/state.params \
  source/

# ケース B の場合: rshogi 形式 → YO 形式に逆変換してから apply
cargo run --release -p tools --bin rshogi_to_yo_params -- \
  --rshogi-params /path/to/rshogi/runs/spsa/<RUN>/state.params \
  --base /path/to/rshogi/tune/suisho10.params \
  --mapping /path/to/rshogi/tune/yo_rshogi_mapping.toml \
  --output /tmp/tuned_yo.params
cd /path/to/YaneuraOu
python3 /path/to/rshogi/tune/tune.py apply /tmp/tuned_yo.params source/
```

`apply` 後、`%%TUNE_DECLARATION%%` 等のマーカーは消え、注入された `TUNE(...)` も実定数に
置換される。production ビルドして完了。元の状態に戻したい場合は `git checkout source/`。

> 注意: `<run-dir>/state.params` は SPSA 反復ごとに上書きされる live ファイル。
> apply 後に同じ run-dir で SPSA 継続 (resume) すると、apply 時点の値とその後の
> SPSA 進行値が乖離する。apply の値を保存しておきたい場合は別ファイルに
> `cp` してから apply する (例: `cp <run-dir>/state.params <run-dir>/applied.params`)。

### 10.7 旧 run dir からの引き継ぎ

旧バージョンの spsa バイナリで作られた run dir (旧 meta 形式や `tuned.params`
ベースのパス命名を持つもの) を、現バージョンで継続したい場合の手順。

#### できること / できないこと

- **できる**: 旧 run の最終 params を新 run の `--init-from` ソースとして使う
- **できない**: `completed_iterations` / `total_games` / SPSA schedule 状態の引き継ぎ
  (新フォーマットの hash 群が旧 meta に存在しないため、resume は不可)

#### 引き継ぎ手順

```bash
# 旧 run の最終 params (tuned.params など) を canonical として保存
OLD_RUN="runs/spsa/20260401_120000_oldrun"
cp "${OLD_RUN}/tuned.params" spsa_params/seed_from_oldrun.params

# 新 run を fresh start (iter は 0 からカウント)
NEW_RUN="runs/spsa/$(date -u +%Y%m%d_%H%M%S)_resumed_from_oldrun"
cargo run --release -p tools --bin spsa -- \
  --run-dir "${NEW_RUN}" \
  --init-from spsa_params/seed_from_oldrun.params \
  --iterations <残りたい iter 数> \
  --games-per-iteration 64 ...
```

#### 注意点

- 新 run の iter 1 は旧 run の続きではなく「旧最終値を初期値とする新規 SPSA」。
  schedule の `c_k` も最初から (大きい摂動) で始まるため、旧 run 末尾と同じ
  収束領域に戻るには数十 iter かかる場合がある。
- 旧 run の stats / values CSV は引き継がない。集計が必要なら手動で結合する。
- 「完全に途中から再開」が必要なら、旧 run を作った時点のバイナリで継続するしかない。

## 11. トラブルシューティング

実機での typical な詰まりどころと対処（症状ベース）。

### `engine read timeout` で SPSA が停止する

- byoyomi が短すぎてエンジンが 1 手返す前にタイムアウト判定。`--byoyomi` を伸ばす
  か `--timeout-margin-ms` を増やす
- エンジンが起動時に `isready` で panic している。`echo -e "usi\nisready\nquit" | <engine>`
  を直接叩いて応答を確認
- NNUE 評価関数のロード失敗（パス間違い、サイズ不正、進行度ファイル未指定 etc.）。
  `--usi-option` の指定漏れを疑う

### `option name X` を `setoption` しても値が変わらない

- エンジンが当該 `X` を USI option として公開していない。`echo usi | <engine> | grep "option name X"`
  で確認
- YO 駆動時で `X` が rshogi 命名のままなら `--engine-param-mapping` を渡し忘れ
- mapping 表の `unmapped.rshogi` に入っている rshogi 専用 param は YO 側に対応する option がない
  ので setoption しても無視される（仕様）

### 変換結果が `min/max` を超える警告 (`out_of_range`)

- `yo_to_rshogi_params` / `rshogi_to_yo_params` の出力で発生。`--base` で渡した
  `.params` の range が転記元値に対して狭い場合に出る
- `--strict-range` を付けるとエラーで止まる。一時的に `--base` の range を広げるか、
  該当 param を `unmapped` に移して翻訳対象から外す

### `init/resume 設定エラー: --init-from が指定されていますが ... は既に存在します`

`<run-dir>/state.params` が既存の run-dir に存在し、かつ `--init-from` も
指定されているが `--resume` / `--force-init` が指定されていない時の安全 bail。
解決策は 3 通り:

- **続行したい** → `--resume` を追加 (canonical との整合性 diagnostic も出力される)
- **既存を破棄して canonical から作り直したい** → `--force-init` を追加
- **そもそも指定ミス** → 既存ファイルを `rm` するか、`--run-dir` を新規 timestamped dir にする

推奨運用は **`--run-dir runs/spsa/$(date -u +%Y%m%d_%H%M%S)_<tag>`** のように
毎回 timestamped dir を切ること。

### `meta format version 不一致`

`meta.json` の `format_version` が現バージョンの想定と異なる場合に出る。
旧形式の `meta.json` を持つ run dir は resume 不可なので、新規 run dir で
`--init-from <canonical>` から fresh start する (旧 run の最終値を引き継ぎたい
場合は §10.7 を参照)。

### `param 名集合が meta と不一致です`

- mapping 表に param を追加 / 削除した状態で過去 run を resume しようとした場合に発生
- 現状 escape hatch なし。新規 run-dir で fresh start すること

### `info: N param(s) matched --active-only-regex but are unmapped`

- `--engine-param-mapping` 指定時、regex マッチしたが mapping 表に無い rshogi 名がある
- 想定通りの「YO 側に対応がない」rshogi 専用 param なら無視で OK
- 想定外なら mapping 表の追加を検討。`check_param_mapping --yo-binary` で YO 側の対応
  option があるか確認

### `check_param_mapping --yo-binary` で「YO 側にあるが mapping/unmapped.yo に無い」

- tune.py 注入が増えた可能性。新規 USI option の名前を `unmapped.yo` に追記するか、
  rshogi 側に対応 param があれば `[[mapping]]` を追加
- 条件付き注入（ビルド設定で有無が変わる）の場合は YO ビルド構成の差を疑う

### SPSA が収束しない (`avg_abs_update` がいつまでも下がらない)

- 開始局面の多様性不足。`--startpos-file` を増やす or 切り替える
- `--games-per-iteration` が小さすぎてノイズが大きい。倍に増やす
- `--seeds` が単一。複数 seed (3〜5) にして集計分散を確認
- チューニング対象が広すぎる。`--active-only-regex` でグループ別に分割して順次チューニング
