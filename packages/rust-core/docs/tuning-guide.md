# 探索パラメータ調整ガイド（運用・計測用）

この文書は、探索/時間管理に関する調整パラメータと推奨スイープ、計測観点をまとめた運用ノートです。
本書では具体的な調整手順・観測指標・スクリプト運用にフォーカスします。

## 対象・前提
- 対象ワークスペース: `packages/rust-core`
- ビルド: `cargo build -p engine-usi --release`
- ログは USI 標準出力（`info`行）を収集します。スクリプトは `.smoke/`, `.tune/` に成果物を書き出します。

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
  - ログ例（info string）
    - `near_final_zero_window=1 budget_ms=.. budget_qnodes=.. t_rem=.. qnodes_used=.. confirmed_exact=0|1`


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
