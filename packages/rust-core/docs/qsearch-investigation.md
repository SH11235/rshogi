# 静止探索停滞の分析と修正計画（2025-10-05）

## 現状サマリ

- latest `taikyoku_log.md` では **全ての go が depth=1 でソフトリミットに到達**。例: `time 28.4s / nodes ≈1.3M / seldepth ≧33`。
- `tt_summary` は **hit_pct ≈ 0%**。AB が第 2 反復に進めないため、TT 再利用が効いていない。
- `info string qsearch_deep ply=13 …` が診断ログで確認でき、静止探索が王手・取り合い連鎖で暴走している。
- TimeManager は正常に働き、soft/hard を守って終了。根因は **qsearch の暴走**。
- byoyomi 30 秒でも TimeManager は `soft_ms ≈ 23.7s / hard_ms ≈ 28.95s` と十分余裕を残しているが、qsearch がその枠を使い切っている。

## 影響

- ルートの評価が「深さ 1 + 長大な静止探索の末尾部分」に限定される → 短期的な駒得だけを狙う不自然な手（例: `B*4d`）を選択。
- qsearch 活動ノードが多すぎるため、スレッド 1 でも byoyomi 30s では深さ 2 以降に進めない。TT・ヒューリスティクス全般が効かない状況。

## 即効性のある切り分け（オプションのみ）

| 設定 | 目的 |
|------|------|
| `setoption name QSearchChecks value Off` | quiet check 生成を完全停止。深さ 2 に入るか確認 |
| `setoption name SearchParams.QS_MAX_QUIET_CHECKS value 2` | オフが過激な場合の上限制限 |
| `setoption name ByoyomiDeadlineLeadMs value 0`<br>`setoption name StopWaitMs value 150` | qsearch の join まで余裕を持たせ、第 2 反復に入るか観察 |
| `setoption name SearchParams.EnableProbCut value true` | Profile 初期値で外れている場合に浅層枝を刈る |

結果を `tt_snapshot`／`tt_summary`／`bestmove` の深さで比較し、静止探索以外の要因がないか切り分ける。

## 診断ログの強化（実装済み）

- `qsearch_deep` ログを追加済み（ply>=12 で一度だけ出力）。
- 追加予定のログ: quiet check 生成件数、SEE 判定結果、チェック連鎖で使っている手。

## 根本対策（優先順）

1. **qsearch 時間・ノード上限の導入**  
   - `SearchContext` に `qnodes` を追加し、`DEFAULT_QNODES_LIMIT` を実際に適用。超過時は `return alpha`。
   - あるいは qsearch 内の `ctx.time_up()` を高頻度に回す（`(*ctx.nodes & 0x3FF)==0` など）。

2. **quiet checks の抑制**  
   - `QS_MAX_QUIET_CHECKS` を小さめに（例: 0 or 2）。
   - `stand_pat + check_bonus <= alpha` ならチェック生成をスキップ。EE の王手生成を `MovePicker` 側でフィルタ。

3. **捕獲枝の delta/SEE カット強化**  
   - `see < 0` かつ `captured_val < 500` の捕獲は捨てる。
   - `QS_MARGIN_CAPTURE` を増やし（100→150+）、`stand_pat + margin <= alpha` で早期 continue。

4. **時間ポーリング頻度向上**  
   - `SearchContext::time_up()` のマスクを静止探索だけ半分に下げる or qsearch 内で別マスクを使う。

5. **ProbCut / Razor / IID の整合**  
   - Profile で無効化されている pruning を見直し、浅層枝の削減を再有効化する。

6. （必要なら）**TT ストアフィルタの緩和**  
   - `should_skip_tt_store_dyn()` の hashfull 閾値を調整し、浅層でも保存できるようにする。

## 実装タスク（Draft）

1. `SearchContext` に `qnodes` と `DEFAULT_QNODES_LIMIT` を導入。`tick`／`qsearch` に反映。
2. qsearch 内 `ctx.time_up()` 呼び出しを高頻度にする。`time_manager.should_stop` を直接参照する分岐を追加。
3. `QS_MAX_QUIET_CHECKS` と SEE カットの閾値を調整。USI から上書き可能に維持。
4. `qsearch.rs` に追加診断ログ（チェック連鎖の手順、SEE 判定）を `#[cfg(feature="diagnostics")]` で挿入。
5. ProbCut を Profile で有効化（`ClassicBackend::with_profile_and_tt` で `profile.apply_runtime_defaults()` を適切な位置で呼ぶ）。
6. オプションを組み合わせたリグレッション（`scripts/tt_multi_go.py`）で 1〜6手目をカバーし、深さ 2 への進行と処理時間を記録。

## 受け入れ条件

- `position startpos moves 7g7f 4a3b …` + `go btime 0 wtime 0 byoyomi 30000` で **深さ 2 以上に進み、seldepth が 20 以下に収束**。
- 同局面を `DEPTH=10` で回した結果と `go byoyomi` の結果が整合し、不自然手が減少する。
- `tt_summary hit_pct` が 0% から改善（最低でも >0.5% 程度）。
- `qsearch_deep` ログが soft deadline 手前で繰り返し出ない（必要時のみ）。

進捗は本ドキュメントに追記しながら進める。

## 進捗メモ（2025-10-05）

- `qsearch` にノード上限・高速タイムチェックを導入。`DEFAULT_QNODES_LIMIT = 300_000`。
- quiet check の最大数を 4→実質 2 手前後に抑制、SEE<0 捕獲に margin 判定を追加。
- `SearchProfile::basic*` でも ProbCut を有効化し、浅層枝が削減されるよう調整。
- 診断ログ `qsearch_deep` で静止探索暴走を 1 度だけ可視化（ログ済み）。
- 実局面（byo30）の `go` で `iter_start depth=10` まで確認、`tt_summary hit_pct=27.45`。`deadline_hit` による深さ1停滞は解消済み。
