# 探索パラメータ調整ガイド（運用・計測用）

この文書は、探索/時間管理に関する調整パラメータと推奨スイープ、計測観点をまとめた運用ノートです。
本書では具体的な調整手順・観測指標・スクリプト運用にフォーカスします。

## 対象・前提
- 対象ワークスペース: `packages/rust-core`
- ビルド: `cargo build -p engine-usi --release`
- ログは USI 標準出力（`info`行）を収集します。スクリプトは `.smoke/`, `.tune/` に成果物を書き出します。

## ターゲットデータセットと構造化ログ

探索パラメータ調整・SPSA の評価は、最終的に「`targets.json` + `summary.json`」のペアに集約します。  
`targets.json` の `targets[].pre_position` は `run_eval_targets.py` が USI `position` として読み、各プロファイルで再評価します。

- 主なデータソース
  - 外部 GUI/サーバの対局ログ（`taikyoku-log/taikyoku_log_enhanced-parallel-*.md`）
  - 内部 gauntlet（`usi_gauntlet`）の構造化ログ（`moves.jsonl`）

### 外部 USI ログからのターゲット生成（概要）

詳細な手順は AGENTS.md の「4. ログ分析ワークフロー」を参照してください。ここでは流れのみ整理します。

1. 落下候補の抽出  
   - `scripts/analysis/extract_eval_spikes.py` で最新 USI ログから評価スパイクを検出し、スパイク ply 付近のプレフィクスを抽出。
2. 局面列の再生・プレフィクスごとの再評価  
   - `scripts/analysis/replay_multipv.sh` で MultiPV を指定して短時間再生し、`runs/game-postmortem/...` に `summary.txt` を集約。
3. ターゲット化（back=2..5）  
   - `scripts/analysis/make_targets_from_logs.py` もしくは `extract_positions_from_log.py` + `expand_targets_back.py` を用いて、
     落下点から数手遡った `pre_position` 群を `targets.json` にまとめる。

この経路は「外部で既にたくさん対局してしまったログを後から使いたい」場合に有効です。

### 自己対局ログ（selfplay_basic）からのクイック診断

`selfplay_basic` で生成される `runs/selfplay-basic/*.jsonl` と同名 `.info.jsonl` は、Rust 製ツールでそのまま解析できます。典型的な運用は次のとおりです。

0. **自己対局の実行（例）**
   ```bash
   cargo run --release -p tools --bin selfplay_basic -- \
     --games 1 \
     --max-moves 180 \
     --think-ms 5000 \
     --threads 8 \
     --basic-depth 2
   ```
   Black=本エンジン、White=ShogiHome簡易エンジンで対局し、`runs/selfplay-basic/` に JSONL / `.info.jsonl` / `.kif` を生成します。

1. **悪手抽出 + ターゲット生成**  
   ```bash
   cargo run -p tools --bin selfplay_blunder_report -- \
     runs/selfplay-basic/<log>.jsonl \
     --threshold 400 \
     --back-min 0 \
     --back-max 3
   ```
   - `threshold`: 隣接する `main_eval` の評価差がこの値以下になった手をブランダー候補とみなす（負の差のみカウント）。
   - `back-min/back-max`: spike から遡る手数の範囲。バック値ごとに `pre_position` を生成し、`targets.json` にまとめます。
   - 出力: `runs/analysis/<log>-blunders/{blunders.json,targets.json,summary.txt}`。`blunders.json` には SFEN・指し手・info 行抜粋が含まれるので、そのまま調査に利用できます。

2. **ターゲットの再解析（Multi Profile）**  
   ```bash
   cargo run -p tools --bin selfplay_eval_targets -- \
     runs/analysis/<log>-blunders/targets.json \
     --threads 8 \
     --byoyomi 2000
   ```
   - `engine-usi` を `base/rootfull/gates` の 3 プロファイルで再実行し、`summary.json` と `*__<profile>.log` を生成。
   - `summary.json` には `origin_log` / `origin_ply` / `back_plies` が記録され、どの局面を再評価したのか一目で分かります。

これにより「自己対局 → ブランダー抽出 → 遡り局面での再解析」というループを Rust CLI だけで回せます。従来の Python スクリプト（`extract_eval_spikes.py` 等）を使う必要はありません。

### usi_gauntlet + moves.jsonl からのターゲット生成

内部 A/B や教師エンジンとの大量対局では、`usi_gauntlet` の構造化ログ機能を使うと配管が簡単になります。

1. 構造化ログ付きで gauntlet 実行  
   ```bash
   cargo run -p tools --bin usi_gauntlet --release -- \
     --engine target/release/engine-usi \
     --base-init runs/gauntlet_usi/init/base_init.usi \
     --cand-init runs/gauntlet_usi/init/cand_init.usi \
     --book runs/gauntlet_usi/short20.sfen \
     --games 20 --byoyomi-ms 500 \
     --engine-threads 8 --hash-mb 1024 \
     --concurrency 1 --adj-enable \
     --log-moves \
     --out runs/gauntlet_usi/20251114-1136-ab-500ms
   ```
   - 出力: 上記ディレクトリに `games.csv` / `summary.txt` / `result.json` に加え、1 手ごとの `moves.jsonl` が生成されます。
   - `moves.jsonl` の主な列:
     - `game_index, ply, stm_black, side, cand_black, open_index, sfen`
     - `position`（`startpos ...` / `sfen ... moves ...` の本体）
     - `bestmove, eval_cp, eval_mate`

2. `moves.jsonl` から `targets.json` を生成  
   ```bash
   python3 scripts/analysis/make_targets_from_moves.py \
     runs/gauntlet_usi/20251114-1136-ab-500ms/moves.jsonl \
     --threshold 250 \
     --topk 10 \
     --back-min 2 \
     --back-max 5 \
     --side cand \
     --out runs/20251114-tuning-from-gauntlet
   ```
   - `threshold`: 隣接 eval 差分（cp）の絶対値がこの値以上の ply をスパイクとみなす。
   - `back-min` / `back-max`: スパイク直後から何手遡るかの範囲。
   - `side`: `"cand"` / `"base"` / `"both"`。悪手抽出の目的上、通常は `"cand"`。
   - 出力: `<out>/targets.json` / `<out>/summary.txt`。

3. 再評価・指標算出  
   - `ENGINE_BIN=target/release/engine-usi python3 scripts/analysis/run_eval_targets.py <out> --threads 8 --byoyomi 10000 --minthink 100 --warmupms 200`
   - `python3 scripts/analysis/summarize_true_blunders.py <out>`
   - `python3 scripts/analysis/summarize_drop_metrics.py <out> --bad-th -600`
   - `scripts/analysis/run_ab_metrics.sh --dataset <out> --out-root runs/<YYYYMMDD>-ab ...`

`moves.jsonl` 経路は、外部ログを介さず「gauntlet → targets.json → run_eval_targets.py → A/B メトリクス」まで一気通貫で回せるのが利点です。

## 主要パラメータ（USI オプション）
- 時間ポリシー/締切関連
  - `OverheadMs`（既定50）: 送受信/GUI遅延のベース上乗せ。
  - `ByoyomiOverheadMs`（= `network_delay2_ms`、既定800）: 追加ネット遅延。純秒読みの締切前倒しにも寄与。
  - `ByoyomiDeadlineLeadMs`（既定300）: 純秒読みで GUI 締切より前倒しするリード。
  - `ByoyomiSafetyMs`（既定500）: ハード上限の安全差し引き。
  - `MinThinkMs` / `PVStabilityBase` / `PVStabilitySlope` / `SlowMover` / `MaxTimeRatioPct` / `MoveHorizon*`:
    伸ばし/収束ポリシーの調整。短手計測で効果を観測。
 - `StopWaitMs`（既定0）: `stop` 経路の待機合流バジェット（OOBと併用可）。

## Runtime トグル（探索側の環境変数）

運用時に挙動を軽く切り替えられる探索側の環境変数（既定はすべてOFF/保守的）。

- SHOGI_LEAD_WINDOW_FINALIZE（既定: 1）
  - リードウィンドウ（Soft）での穏やかな停止を有効にする。

- SHOGI_DISABLE_STABILIZATION（既定: 0）
  - 安定化ゲート群（近締切ゲート/アスピ安定化/狭窓検証の枠）を一括で無効化（旧名 `SHOGI_DISABLE_P1` も受理）。

- SHOGI_QNODES_LIMIT_RELAX_MULT（既定: 1, 範囲: 1..32）
  - `compute_qnodes_limit` の最終上限（DEFAULT）を倍率で緩和。長TC/分析モードで SelDepth を伸ばす A/B に使用。

- SHOGI_ZERO_WINDOW_FINALIZE_NEAR_DEADLINE（既定: 0）
  - Near‑hard 〆切帯で PV1 に対する「狭窓検証」（[s−Δ, s+Δ]、既定 Δ=1cp）を 1 回だけ実施し、Exact を確認。
  - 併用パラメータ（任意）
    - SHOGI_ZERO_WINDOW_FINALIZE_VERIFY_DELTA_CP（既定 1, 範囲 1..32）
    - SHOGI_ZERO_WINDOW_FINALIZE_BUDGET_MS（既定 80, 範囲 10..200）
      - 壁時計予算（ms）。内部で `budget_qnodes` に換算し、ローカル qnodes 上限にクランプします。
    - SHOGI_ZERO_WINDOW_FINALIZE_MIN_DEPTH（既定 4, 範囲 1..64）
      - 実施する最小反復深さ。浅い反復では実行しない。
    - SHOGI_ZERO_WINDOW_FINALIZE_MIN_TREM_MS（既定 60, 範囲 5..500）
      - 残時間がこの値未満なら実行しない（極小時間での回し直し抑止）。
    - SHOGI_ZERO_WINDOW_FINALIZE_MIN_MULTIPV（既定 0）
      - MultiPV がこの値未満なら検証を行わない（高MPV時のみONにする運用向け）。
    - SHOGI_ZERO_WINDOW_FINALIZE_SKIP_MATE（既定 0）
      - mate帯スコア近傍では検証をスキップ（距離ゆらぎ対策）。
    - SHOGI_ZERO_WINDOW_FINALIZE_MATE_DELTA_CP（既定 0, 0..32）
      - mate帯では Δ をこの値だけ追加で広げる（skipよりもExact化優先したい場合）。
  - ログ例（info string）
    - 実行: `near_final_zero_window=1 budget_ms=.. budget_qnodes=.. qnodes_limit_pre=.. qnodes_limit_post=.. t_rem=.. qnodes_used=.. confirmed_exact=0|1`
    - スキップ: `near_final_zero_window_skip=1 reason=already_exact|min_depth|trem_short|min_multipv|mate_near ...`

- MultiPV スケジューラ（最小）
  - `SHOGI_MULTIPV_SCHEDULER`（既定: 0/Off）
    - 有効化すると PV1 を優先し、PV2 以降の qsearch 上限を強めに絞る（`compute_qnodes_limit` 内）。
  - `SHOGI_MULTIPV_SCHEDULER_PV2_DIV`（既定: 4、範囲: 2..32）
    - PV2 の分配倍率。PVn は概ね `div * n` 相当で強く制限される。高MPV×長TCの注釈で PV1 の確定性を上げる用途。

- 浅層ゲート（任意の安定化）
  - `SEARCH_SHALLOW_GATE`（既定: 0/Off）
    - ルート浅層（例: d≤3）で ProbCut/NMP を抑制し、LMR を弱める。PV が立たない源流を軽減する運用向け。
  - `SEARCH_SHALLOW_GATE_DEPTH`（既定: 3）
    - 浅層ゲートを適用する深さ上限。
  - `SEARCH_SHALLOW_LMR_FACTOR_X100`（既定: 120）
    - 浅層での LMR 係数（%）。値を大きくすると減深が弱まる（=安定寄り）。

## Diagnostics 指標（CSV/JSON 対応）
- classicab_diagnostics（format=csv|json）における主な列
  - 収率/境界: `lines_len`, `top1_exact`
  - 近締切: `near_deadline_skip_new_iter`, `multipv_shrunk`
  - 狭窓検証: `near_final_attempted`, `near_final_confirmed`
  - パフォーマンス: `nodes`, `nps`, `tt_hits`, `lmr`, `lmr_trials`, `beta_cuts`

## A/B Tips（運用例）
- P0+P1の評価（短TC/浅深）
  - 例: TIME=300ms, MPV=2, HASH=512, JOBS=16。`SEARCH_SHALLOW_GATE=1`（d≤3）, `SHOGI_ZERO_WINDOW_FINALIZE_BOUND_SLACK_CP=1` を併用し、空PV≈0%、Top1Exact の伸び/NPS 変動（±2%以内）を確認。
- 高MPV 束のみ MultiPV スケジューラON
  - 例: MPV≥7, TIME≥800ms。`SHOGI_MULTIPV_SCHEDULER=1`, `SHOGI_MULTIPV_SCHEDULER_PV2_DIV=4` から開始。PV1 確定性↑と PV2+ 遅延のトレードオフを比較。


- 探索パラメータ（runtime）
  - `SearchParams.LMR_K_x100`（既定170）: LMR の強さ係数（低いほど強めに減深）。
  - `SearchParams.LMP_D{1,2,3}`: Late Move Pruning の深さ別しきい。
  - `SearchParams.ProbCut_{D5,D6P}`: ProbCut マージン（深さ依存）。
  - `SearchParams.IID_MinDepth`（既定6）: IID を開始する最小深さ。
  - `SearchParams.Razor` / `SearchParams.SBP_*` / `SearchParams.HP_Threshold`: Razoring/静的枝刈り/静的評価閾。
  - `QSearchChecks`（On/Off）: QS で王手手を許可するか（ビルド時/環境変数の両経路あり）。

- その他
  - `MultiPV`（1..20）: ライン数。調整時は 1 を推奨（安定性/比較容易性のため）。
  - `Threads` / `USI_Hash`: NPS/TT挙動に影響。単体比較は 1 thread から。

## OOB finalize（USI 側）調整項目
- 期限監視: `deadline_hard`（＋任意 `deadline_near`）でメインループ毎に監視し、hard到達で `fast_finalize`。
- 最小深さ閾: `FAST_SNAPSHOT_MIN_DEPTH_FOR_DIRECT_EMIT`（既定3）
  - 閾下では TT で 1–2ms のプローブ予算を許可して補強。
- near-hard の扱い（現状ログのみ）
  - 締切が厳しいGUI/サーバ向けには「near-hard到達で fast finalize」をオプション化する余地。

## 推奨スイープと観測指標
- 代表スイープ（固定レンジ例）
  - `LMR_K_x100`: 160, 170, 180
  - `ProbCut_D6P`: 300, 320
  - `IID_MinDepth`: 6 → 5
- 指標（`finalize_diag`/USIログから取得）
  - `nodes / nps / tt_hits / root_fail_high_count`
  - `aspiration_fail/aspiration_hit`
  - `lmr`（適用回数）/`lmr_trials`（試行回数）
  - `seldepth`、`helper_share_pct`（ヘルパー比。USIログでは `helper_share_pct` として出力）
  - 最終 `info` 行の bound が Exact（`lowerbound`/`upperbound` が含まれない）

## スクリプト運用
- スモーク
  - 近ハード・ゲート: `bash scripts/smoke_near_hard_gate.sh`
    - 近ハード到達警告→hard OOB finalize 発火、次反復未突入（最大深さ+1の `info depth` 不在）、最終行 Exact を確認（OOB直出しでPV行が省略される場合は警告のみ）。
  - 最終行 Exact: `bash scripts/smoke_bound_exact.sh`
  - 純秒読み OOB 発火: `bash scripts/smoke_oob_enforce.sh`
  - 余裕時間あり・通常合流: `bash scripts/smoke_normal_join.sh`
- スイープ
  - `bash scripts/sweep.sh .tune/sweep_results.csv`
  - スイープ値はスクリプト先頭の配列（`LMR_LIST`/`PC6P_LIST`/`IID_LIST`）を編集して調整。
  - 出力CSV: `preset,depth,lmr_k_x100,probcut_d6p,iid_min_depth,nodes,nps,tt_hits,root_fail_high,asp_fail,asp_hit,lmr,lmr_trials,seldepth`

## NNUE 前探索パラメータ調整フロー（概要）

NNUE 学習前の探索パラメータ調整では、「大駒タダ取り・評価急落（スパイク）を抑えること」を主目的に、
バンドル単位でパラメータを触りつつ `targets.json` データセットで悪手回避率を見ます。

### バンドル別調整ステップ（例）

1. LMR/LMP/統計リダクション塊  
   - 対象: `SearchParams.LMR*`, `SearchParams.LMP_D*`, `SearchParams.SameToExtension`, `SearchParams.RootBeamForceFullCount` など。
   - 手順: 既定値 → LMR ゲートをやや緩める → `run_eval_targets.py --profile base --threads 8 --byoyomi 1000` で depth/NPS/落下局面の PV を確認。

2. NMP / ProbCut / IID / StaticBeta 塊  
   - 対象: `SearchParams.Enable*`, `Search.NMP.Verify*`, `SearchParams.ProbCut_*`, `SearchParams.IID_MinDepth`。
   - 手順: バンドル全体を OFF に振ってターゲットを再評価 → 効き過ぎていないか確認し、必要なものだけ戻す。

3. Futility / SafePruning / SEE ガード塊  
   - 対象: `SearchParams.FUT_*`, `SearchParams.SBP_*`, `SearchParams.SafePruning`, `Search.CaptureFutility.*`, `SearchParams.QS.*`, `Search.QuietSeeGuard`。
   - 手順: SEE 閾値や Futility マージンを緩め、`run_eval_targets.py --profile gates` や `run_dropguard_regression.sh` で Threat2 / Drop guard セグメントを確認。

4. Finalize / MateGate / InstantMate 塊  
   - 対象: `FinalizeSanity.*`, `MateGate.*`, `InstantMateMove.*`, `FailSafeGuard`。
   - 手順: SEE/Threat2 閾値や MateProbe を調整し、「探索の取りこぼしを最終段でどこまで救えるか」を確認（ただし AGENTS 方針どおり“安全弁”として扱う）。

5. ヒストリ / Root バイアス塊  
   - 対象: `SearchParams.(Capture|Continuation|Quiet)HistoryWeight`, `SearchParams.RootTTBonus`, `RootPrevScoreScale`, `RootMultiPV*`。
   - 手順: 履歴重みや Root バイアスを一段落として A/B し、「過去統計に引きずられて悪手を選んでいないか」を見る。

各ステップごとに `runs/<YYYYMMDD>-tuning` などのディレクトリを分けて `targets.json` / `summary.json` / 各種メトリクスを保存しておくと、後から比較しやすくなります。

### first_bad / avoidance を用いた評価

落下局面の評価には「spike 率」だけでなく、`first_bad` と悪手回避率（avoidance）を併用します。  
詳細な定義と運用コマンドは AGENTS.md の「5. 計測指標（first_bad/avoidance）と A/B 運用」を参照してください。

- 典型的な流れ:
  1. `scripts/analysis/pipeline_60_ab.sh` でログから 60 件程度のターゲットデータセットを作成。
  2. `run_eval_targets.py` / `run_ab_metrics.sh` で各プリセットを評価し、`metrics.json` / `metrics_first_bad.json` / `avoidance.json` を生成。
  3. `summarize_true_blunders.py` / `summarize_avoidance.py` / `summarize_drop_metrics.py` で真の悪手ビューや落下率・回避率を可視化。
- 採否の目安:
  - 悪手回避率（avoidance_rate）がベースライン以上に改善しているか。
  - avg_depth / NPS が許容範囲（極端に浅くならないか）。
  - overall spike は大幅悪化のみ警戒（多少の揺れは許容）。

### NNUE 前 SPSA（落下率最小化）の位置付け

NNUE 学習前でも SPSA は利用できますが、目的関数は「短TC勝率」ではなく「落下率/悪手回避」に寄せます。

- 対象パラメータ（例）
  - QS/SEE: `SearchParams.QS_CheckPruneMargin`, `QS_MarginCapture`, `QS_BadCaptureMin`, `QS.CheckSEEMargin`。
  - Futility/SBP: `SearchParams.FUT_Dyn_*`, `SearchParams.SBP_Dyn_*`。
  - LMR/LMP: `SearchParams.LMR_K_x100`, `SearchParams.LMP_D1/D2/D3`。
- 目的関数イメージ
  - 主目的: SpikeRate（|Δeval| ≥ 800cp の発生率）や first_bad 限定スパイク率の低減。
  - 罰則: NPS の大幅悪化を防ぐため、`f = -SpikeRate + α·log(NPS_ratio)` のような形を採用。
- 運用メモ
  - 離散 ON/OFF を多数混ぜると SPSA が不安定になるため、まずは連続値パラメータ 6〜10 個程度に絞る。
  - 極端な「全バンドル OFF」設定での SPSA は避け、常に「実用ベースライン ±α」の範囲で調整する。

## 調整バックログ（将来の論点）
- リード（deadline lead）の一元化
  - 現状: USI 側で `ByoyomiDeadlineLeadMs` を `network_delay2_ms` に加算＋探索側は `soft/hard` 差分でゲート。
  - 案: リードはUSI側で一元設定し、探索側は「`soft` が存在する場合は `soft` で止める」へ寄せる（調整点の単純化）。
- `FAST_SNAPSHOT_MIN_DEPTH_FOR_DIRECT_EMIT` の再評価（3→4 も選択肢）。
- near-hard での fast finalize 発火をオプション化。
- TT プローブ予算関数 `compute_tt_probe_budget_ms` の係数見直し（remain/10→可変、最小1ms維持）。
- MultiPV>1 の OOB fast finalize での表示整合（必要なら最終PV行の強制出力パスを追加）。

## 参考（USI→内部マッピング）
- USI オプション→内部設定
  - 時間関連: `OverheadMs`/`ByoyomiOverheadMs`→`TimeParameters`、`ByoyomiDeadlineLeadMs`→`network_delay2_ms` に上乗せ、`StopWaitMs`→OOB待機合流。
  - 探索関連: `SearchParams.*` → `engine_core::search::params::*` setter を直呼び。
- ログ項目
  - `finalize_snapshot/time_caps/finalize_diag/tt_debug`、`oob_*`、`info depth/seldepth/hashfull/nodes/nps` など。
