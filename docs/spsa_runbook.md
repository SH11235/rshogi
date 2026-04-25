# SPSA実行コマンド一式

## 前提
- リポジトリルートで実行する。
- `rshogi-usi` と `tools/spsa` をローカルビルド可能であること。

## 1. 初回ビルド

```bash
cargo build --release -p rshogi-usi
cargo build --release -p tools --bin generate_spsa_params --bin spsa --bin spsa_stats_to_plot_csv
```

## 2. 実行ディレクトリ作成 + `.params` 自動生成

```bash
RUN_ID=$(date -u +%Y%m%d_%H%M%S)
RUN_DIR="runs/spsa/${RUN_ID}"
mkdir -p "${RUN_DIR}"
cargo run --release -p tools --bin generate_spsa_params -- \
  --output "${RUN_DIR}/tuned.params"
```

## 3. SPSA実行（更新）

```bash
cargo run --release -p tools --bin spsa -- \
  --params "${RUN_DIR}/tuned.params" \
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

CSV 出力先は既定で `${RUN_DIR}/tuned.params.{stats,stats_aggregate,values}.csv`
に自動生成される。別パスにしたい場合のみ `--stats-csv` / `--stats-aggregate-csv`
/ `--param-values-csv` を明示。生成を止めたい場合は `--no-*` フラグを使う。

集計CSV (`stats_aggregate`) のパス導出規則:

- `--stats-aggregate-csv` 明示 → そのパスを使用
- `--stats-csv <S>` 明示（明示なし）→ 互換性のため従来の派生 `<S>.aggregate.csv`
  を使う。これにより既存ジョブを `--resume` した時に既定の集計CSV出力先が変わらない
- どちらも未指定 → 既定の `<params>.stats_aggregate.csv`
- seeds 指定が単一のときは集計CSV を生成しない

## 4. 再開実行（resume）

```bash
cargo run --release -p tools --bin spsa -- \
  --params "${RUN_DIR}/tuned.params" \
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

スケジュール設定を変更して再開する場合だけ `--force-schedule` を付与する。

## 5. 可視化用CSV変換（任意）

```bash
cargo run --release -p tools --bin spsa_stats_to_plot_csv -- \
  "${RUN_DIR}/tuned.params.stats.csv" \
  --output-csv "${RUN_DIR}/tuned.params.stats.plot.csv" \
  --window 16

cargo run --release -p tools --bin spsa_stats_to_plot_csv -- \
  "${RUN_DIR}/tuned.params.stats_aggregate.csv" \
  --output-csv "${RUN_DIR}/tuned.params.stats_aggregate.plot.csv" \
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
- 早期停止を有効化する: `--early-stop-avg-abs-update-threshold` / `--early-stop-result-variance-threshold` / `--early-stop-patience`
- 速度優先の暫定実行: `--games-per-iteration` と `--iterations` を小さくする

## 9. 生成物

`--params <path>` を基準に、以下が既定で自動生成される（PR #481）:

| 生成物 | 既定パス | opt-out フラグ |
|---|---|---|
| 更新済みパラメータ | `<params>` (上書き) | — |
| 再開メタデータ | `<params>.meta.json` | — |
| seed単位統計 | `<params>.stats.csv` | `--no-stats-csv` |
| seed集計統計 (複数 seed 時のみ) | `<params>.stats_aggregate.csv` | `--no-stats-aggregate-csv` |
| パラメータ履歴 | `<params>.values.csv` | `--no-param-values-csv` |
| 可視化CSV (`spsa_stats_to_plot_csv` 出力) | 任意 (`--output-csv`) | — |

明示的に `--stats-csv` / `--stats-aggregate-csv` / `--param-values-csv` を指定した場合は
そのパスが優先される（明示パスが既定パスを上書き）。例えば `--params runs/spsa/<ts>/tuned.params`
を渡せば、CSV はすべて同ディレクトリに `tuned.params.{stats,stats_aggregate,values}.csv`
として置かれる。

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

#### 10.6.1 ケース A: YO 形式 `.params` で YO を直接チューニング（推奨・最も単純）

`.params` の name 列が YO のビルド済み USI option 名と一致していれば、rshogi の `spsa`
はその name を `setoption` にそのまま流すだけなので、**変換ツール・マッピング表は不要**。

> **正本ファイルの上書き保護**: `--params` に渡したファイルは反復ごとに上書きされる。
> 正本（例: `tune/suisho10.params`）を直接渡さず、必ず timestamped run dir の中の
> `tuned.params` を渡す。`--init-from <canonical>` を使うと、`--params` のパスが存在
> しない時に限り canonical をコピーしてから開始する（既存なら resume として読み込む）。

```bash
RUN_DIR="runs/spsa/$(date -u +%Y%m%d_%H%M%S)_yo_suisho10"
mkdir -p "${RUN_DIR}"

cargo run --release -p tools --bin spsa -- \
  --params "${RUN_DIR}/tuned.params" \
  --init-from tune/suisho10.params \
  --engine-path /path/to/YaneuraOu-tune-patched \
  --iterations 200 --games-per-iteration 64 \
  --concurrency 8 --threads 1 --hash-mb 256 --byoyomi 1000 \
  --startpos-file /path/to/openings.txt \
  --seeds 1,2,3,4
```

書き戻された `${RUN_DIR}/tuned.params` は YO 形式のまま運用に乗る。`tune/suisho10.params`
（正本）は触られない。CSV 出力 (`tuned.params.{stats,stats_aggregate,values}.csv`) も
同ディレクトリに自動生成される (§9 参照)。

#### 10.6.2 ケース B: rshogi 形式 `.params` で YO を駆動（`--engine-param-mapping`）

rshogi 側の `.params` フォーマット（`SPSA_*` 命名）のまま YO バイナリを駆動したい場合、
`spsa` バイナリに `--engine-param-mapping` を渡すと、`.params` 内の rshogi 名 (`SPSA_*`)
を `setoption` する直前にエンジン側名前空間（YO 名）に翻訳し、必要なら符号を反転する。

```bash
RUN_DIR="runs/spsa/$(date -u +%Y%m%d_%H%M%S)_yo_via_rshogi"
mkdir -p "${RUN_DIR}"

cargo run --release -p tools --bin spsa -- \
  --params "${RUN_DIR}/tuned.params" \
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
# ケース A の場合: tuned.params が YO 形式のままなので直接 apply
cd /path/to/YaneuraOu
python3 /path/to/rshogi/tune/tune.py apply \
  /path/to/rshogi/runs/spsa/<RUN>/tuned.params \
  source/

# ケース B の場合: rshogi 形式 → YO 形式に逆変換してから apply
cargo run --release -p tools --bin rshogi_to_yo_params -- \
  --rshogi-params /path/to/rshogi/runs/spsa/<RUN>/tuned.params \
  --base /path/to/rshogi/tune/suisho10.params \
  --mapping /path/to/rshogi/tune/yo_rshogi_mapping.toml \
  --output /tmp/tuned_yo.params
cd /path/to/YaneuraOu
python3 /path/to/rshogi/tune/tune.py apply /tmp/tuned_yo.params source/
```

`apply` 後、`%%TUNE_DECLARATION%%` 等のマーカーは消え、注入された `TUNE(...)` も実定数に
置換される。production ビルドして完了。元の状態に戻したい場合は `git checkout source/`。
