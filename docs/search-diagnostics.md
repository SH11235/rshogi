# 探索の診断カウンタ

`search-stats` 有効時、既存の Search Statistics の末尾に `key=value` を追加する。
USI 出力では従来どおり各行に `info string` が付く。カウンタは探索開始時にリセットする。
追加分は thread-local であり、Threads=1 で使用する。診断版の所要時間は通常版の性能比較に使用しない。

配列添字の意味:

- `*_depth[i]`: 0=depth 1、1=2、2=3〜4、3=5以上、4=qsearch。
- `quiet_*_hist[i]`: 0=0手、1=1〜15、2=16〜31、3=32〜63、4=64〜127、5=128以上。
  値はその手数だった QuietInit の回数。
- `quiet_hand_types_hist[i]`: 手駒の駒種が i 種だった回数（0〜7）。
- `repetition_hist[i]`: 1回の千日手情報更新で実際に比較した距離の個数（0〜7）。
- `nnue_ancestor_hist[i]`: 計算済み accumulator の発見距離。0=現在、1〜7=その ply 数、8=8以上。
  見つからない要求は `nnue_ancestor_missing` に数える。探索を打ち切る前に調べた距離の分布ではない。

## MovePicker

| key | 意味 | 計測関数 |
| --- | --- | --- |
| `quiet_init`、`quiet_init_depth[i]` | quiet を生成・採点した回数（skip_quiets で生成しなかった場合を除く） | `MovePicker::next_move` → `diagnostics::quiet_init` |
| `quiet_n`、`quiet_n_depth[i]`、`quiet_n_hist[i]` | 生成した quiet 全手数と1回ごとの分布 | 同上 |
| `quiet_board`、`quiet_pawn_drop`、`quiet_other_drop` と各 `*_depth[i]` / `*_hist[i]` | n を盤上移動・歩打ち・歩以外の駒打ちに分解 | 同上 |
| `quiet_k`、`quiet_k_depth[i]` | 採点後に `value >= limit` を満たした手数。先頭要素を無条件で sorted 領域に置く実装の個数とは区別する | 同上 |
| `quiet_c`、`quiet_c_depth[i]` | GoodQuiet / BadQuiet が実際に返した手数。合法性検査より前、TT 段の手を含めない | `MovePicker::next_move` |
| `quiet_hand_types_hist[i]` | 生成時の手駒の駒種数 | `diagnostics::quiet_init` |
| `sort_shifts` | 挿入ソート内側 while で要素を1つ右へ動かした回数。全段合算 | `partial_insertion_sort` |
| `capture_init` / `capture_generated` | 通常捕獲段の初期化回数 / 生成手数 | `MovePicker::next_move` |
| `qcapture_init` / `qcapture_generated` | qsearch 捕獲段の初期化回数 / 生成手数 | 同上 |
| `probcut_init` / `probcut_generated` | ProbCut 捕獲段の初期化回数 / 生成手数 | 同上 |
| `evasion_init` / `evasion_generated` | 回避段の初期化回数 / 生成手数 | 同上 |
| `picker_stage[i]` | 各 picker が段に初めて到達した回数。手を返すたびには増やさない | 同上 |
| `lmp_triggered` | LMP により skip_quiets を初めて設定した回数 | `SearchWorker::search_node` |
| `lmp_quiets_suppressed` | LMP が抑止した、生成済みで未返却の quiet 候補数。未生成の手数は含めない | `MovePicker::skip_quiets` |

`picker_stage` の添字は Stage の宣言順:
0 MainTT、1 ProbCutTT、2 CaptureInit、3 ProbCutInit、4 ProbCut、5 GoodCapture、6 QuietInit、
7 GoodQuiet、8 BadCapture、9 BadQuiet、10 EvasionTT、11 EvasionInit、12 Evasion、
13 QSearchTT、14 QCaptureInit、15 QCapture。
`picker_stage[6]` は skip_quiets で生成を省いた到達も含む。

## SEE

`see_<source>_<outcome>` は `Position::see_ge` で数える。
呼び出し側は thread-local の source を設定し、入口で消費する。

| source | 設定する関数・箇所 |
| --- | --- |
| `step14_capture` | `step14_pruning` の capture または gives_check の枝 |
| `step14_quiet` | `step14_pruning` の quiet の枝 |
| `check_bonus` | `MovePicker::score_quiets` の王手ボーナス（低 ply / 通常 ply 合算） |
| `qs_futility` | `qsearch` の alpha - futility_base |
| `qs_margin` | `qsearch` の -78 |
| `picker_capture` | `MovePicker::select_good_capture` |
| `probcut` | `MovePicker::select_probcut` |
| `other` | 上記のタグがない呼び出し |

outcome は `false`=入口で即 false、`true`=入口で即 true、`attackers`=交換ループ用の attackers 計算まで到達。
PASS の入口判定も false / true に含める。この3分類は最終的な SEE の真偽の分布ではない。

| key | 意味 | 計測関数 |
| --- | --- | --- |
| `see_step14_double` | 同じ picker の捕獲選別または王手ボーナスで SEE を呼んだ手について、step14 でも呼んだ回数 | `MovePicker::diag_seen`、`search_node`、`diagnostics::begin_see` |
| `see_qs_double` | 同じ qsearch の同じ手で futility と margin の両方を呼んだ回数 | `qsearch`、`diagnostics::begin_see` |
| `see_zero_unattacked` | 取る駒がなく、現在の占有で to / from の両方に相手の利きがない SEE 呼び出し。駒打ちは from がないので to だけを検査 | `diagnostics::begin_see` |
| `see_capture_undefended` | 取る駒があり、現在の占有で to に相手の利きがない SEE 呼び出し | 同上 |

守りの検査は `attackers_to_color_occ` に現在の占有を渡した読み取りだけ。
pin による合法性を除外しない幾何的な利きであり、移動後の仮想占有の利きではない。
SEE の回数・引数・評価結果・探索の分岐条件は変えない。

## qsearch・詰み・枝刈り

| key | 意味 | 計測関数 |
| --- | --- | --- |
| `qs_loop_nodes` | 手のループまで到達した node 数 | `qsearch` |
| `qs_capture_picked` | picker が返した実際の駒取り手数。合法性検査より前、捕獲なし成りを含めない | 同上 |
| `qs_after_two_picked` | 既に合法手を2手以上カウントした後に picker が返した候補数。3手目候補以降を含み、非合法候補も含む | 同上 |
| `qs_after_two_searched` | 上記のうち do_move に進んだ数 | 同上 |
| `qs_all_capture_futility` | ループ到達時に、非王手で futility_base + 相手の盤上駒の最大価値 <= alpha。玉は捕獲対象外として除外 | 同上 |
| `mate_search_calls` / `mate_search_found` | 通常探索の1手詰め呼び出し / 発見数 | `probe_transposition` |
| `mate_qs_calls` / `mate_qs_found` | qsearch の1手詰め呼び出し / 発見数 | `qsearch` |
| `mate_qs_stand_pat` | その node で1手詰めを呼び、後に stand pat で返った回数 | 同上 |
| `search_depth_nonpositive` | 入口引数 depth <= 0 だった search_node 呼び出し数 | `search_node` |
| `search_preloop_<reason>` | 手のループ前に返った node 数。理由は tt / repetition / declaration / razoring / futility / nmp / probcut / mate1 / other | `search_node`、`diagnostics::PreloopExit::drop` |
| `step14_history` / `step14_futility` / `step14_see` | step14 が手を捨てた理由別の件数。合計は既存の move_loop_pruned に対応 | `step14_pruning` |

`search_preloop_other` は depth<=0 で qsearch に委譲した呼び出し、深さ上限、中断、手数上限、mate distance pruning 等を含む。
`probcut` は small ProbCut も含む。LMP は生成や返却を抑止するため、既存の move_loop_pruned の部分集合ではない。

## 局面更新・TT

| key | 意味 | 計測関数 |
| --- | --- | --- |
| `do_drop` / `do_quiet` / `do_capture` | 通常着手を駒打ち・捕獲なし盤上移動・捕獲に分解 | `Position::do_move_impl` |
| `do_promotion` / `do_king` | 上記と重複する成り / 玉移動の件数 | 同上 |
| `do_null` / `do_pass` | null / PASS の回数 | `do_null_move_with_prefetch` / `do_pass_move_impl` |
| `repetition_comparisons` / `repetition_hist[i]` | 実際に board key を比較した距離数の合計 / 更新ごとの分布 | `update_repetition_info` |
| `tt_probe` / `tt_hit_slot[i]` / `tt_miss` | probe 回数 / occupied な slot 0〜2 の hit / miss。空 entry の短縮キー一致は miss に数える | `TranspositionTable::probe` |
| `tt_store` | 選択 slot への store 回数。内容が同値でも数える | `TTWriter::write` |

## NNUE

静的 LayerStacks と universal の DynamicLayerStacks を対象とする。
HalfKP / HalfKx 専用の別ネットワーク経路は対象外。
両視点で別の経路を使う場合があるため、視点・処理段単位と要求単位を分ける。

| key | 意味 | 計測関数 |
| --- | --- | --- |
| `nnue_acc_requests` / `nnue_acc_reused` | accumulator 更新要求数 / 現在の計算済み accumulator の再利用数。評価 hash hit 等で更新要求自体がない評価は含めない | `LsNetByFt::update_accumulator` の do_update / `DynamicLayerStacksNetwork::ensure` |
| `nnue_acc_fallback` | 祖先からの更新ではなく refresh に落ちた要求数 | 同上 |
| `nnue_ancestor_hist[i]` / `nnue_ancestor_missing` | 計算済み accumulator の発見距離 / 未発見数 | 同上、動的版 `forward_update` |
| `nnue_ft_incremental` | 駒 FT の視点ごとの差分更新回数。複数 ply の適用では各段を数える | FT の update_accumulator / update_accumulator_with_cache / forward_update_incremental、動的版 update_from_previous / forward_update |
| `nnue_ft_finny` / `nnue_ft_full` | valid な Finny entry で refresh した視点数 / bias から全再構築した視点数（invalid Finny entry を含む） | `AccumulatorCacheLayerStacks::refresh_or_cache` / with_psqt、FT の refresh、動的版 refresh_perspective |
| `nnue_ft_diff_rows` | 通常差分更新で足し引きした駒 FT 行数。融合 fast path も含む | FT の差分ループ / try_apply_dirty_piece_fast_impl、動的版 apply_feature_changes / try_apply_dirty_piece_fast |
| `nnue_finny_rows` | valid な Finny entry の差分として足し引きした行数 | キャッシュ refresh、動的版 refresh_perspective |
| `nnue_ft_full_rows` | bias から全再構築するときに足した駒 FT 行数 | FT の refresh、キャッシュ refresh、動的版 refresh_perspective |
| `nnue_threat_full` / `nnue_threat_full_rows` | threat の全列挙し直し回数 / 列挙して足した行数 | FT 内の全 for_each_active_threat_index 呼び出し、動的版 refresh_threat |
| `nnue_threat_incremental` / `nnue_threat_diff_rows` | threat の視点ごとの差分更新回数 / 足し引きした行数 | FT の更新処理 / apply_threat_rows |
| `nnue_eval_ft_incremental` / `nnue_eval_ft_finny` / `nnue_eval_ft_full` | 1回の更新要求内でその駒 FT 経路を使った要求数。複数経路を使えば重複して数える | `diagnostics::NnueRequest::drop` |
| `nnue_eval_threat_incremental` / `nnue_eval_threat_full` | 1回の更新要求内でその threat 経路を使った要求数 | 同上 |

threat の Finny cache 経路はない。universal の既存 threat 更新は全再列挙なので、差分の counter は増えない。
各行数は論理的な特徴行数で、SIMD 命令数や実メモリ転送量ではない。

## 制限

計測は探索結果に使わない固定長配列と読み取りに限定する。feature 無効時は追加コード・追加フィールドを cfg で除外する。
同一深さで通常版との node 数一致を実測する検証は、このカウンタのビルド確認とは別に行う。
追加した利き読み取りやカウンタ更新があるため、時間制限のある探索の木の一致を保証する用途には使わない。

LMP の未生成手数、QuietInit 時点の LMP 残り予算、SEE の閾値関係による省略可能件数、
search の1手詰め呼び出し後の razoring / futility 帰還のクロス集計は出力しない。
