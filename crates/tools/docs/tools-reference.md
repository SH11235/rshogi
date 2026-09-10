# rshogi tools リファレンス

crates/tools/src/bin/ 配下の主要バイナリの一覧と解説。

## 対局・トーナメント

| ツール | 説明 |
|--------|------|
| `tournament` | 複数エンジンの round-robin 並列トーナメント。動的目標変更時の先後交換ペア維持、error ペアを同条件で再対局し、`--seed` による matchup ごとの決定的な開始局面選択、seed 付き meta、JSONL 出力、千日手の自動終局・エンジンへの対局履歴送信に対応（[詳細](tournament.md)） |
| `gensfen` | NNUE 学習用 PSV/pack/hcpe3 教師局面の生成（PSV move16 は実 YaneuraOu 形式、hcpe3 policy は既定 65535 票・温度 100、`--hcpe3-eval-drop-threshold` による候補除外と終局理由/gameInfo 符号化、engine vs engine／NativeBackend、native LS progress 係数、`--keep-tt` による native TT・EvalHash・履歴の対局間保持、千日手裁定、異常終局の全局破棄、宣言勝ち PSV 終端局面、乱択来歴 JSONL 記録 (--omit-diversions で件数のみに省略可、deblunder 非互換)、FV_SCALE override、control.json 動的制御・drain、Windows でも動作可 (親 dir fsync はスキップされ電源断耐性が Unix より弱い)。[詳細](gensfen.md)） |
| `nyugyoku_gensfen` | CSA manifest から入玉アンカー局面を disk-partition exact dedup で抽出し、checkpoint/resume 付きで gensfen 用 `startpos.txt` と provenance を生成（[詳細](nyugyoku_gensfen.md)） |
| `csa_client` | USI エンジンを floodgate 等の CSA サーバーに接続して連続対局 |
| `analyze_selfplay` | 自己対局の JSONL ログを attempt 世代別に集計。勝率・Elo 差・NPS・error 件数等を表示（[詳細](analyze_selfplay.md)、[LLR 計算・再計算時の注意](tournament.md#llr-の計算と保存ログの再計算)） |
| `floodgate_record` | csa_client の per-game JSONL から 1 エンジンの戦績を集計（先後別勝率・相手別・後手勝ち/負け/引分・実戦 NPS、`--config` で csa_client 設定から入力導出、`--fetch-ratings` で wdoor 現在レート併記・履歴記録。floodgate 連続対局向け、[詳細](floodgate_record.md)） |
| `jsonl_to_kif` | tournament 等の JSONL 対局ログから KIF 棋譜を生成（id/skip/limit でフィルタ可） |
| `kifu_player` | PSV / tournament JSONL / CSA を同じ TUI で再生・閲覧（評価値グラフ・検索/絞り込み（SFEN 局面検索含む）・`--live` 追記監視 (live-mirror と組で wdoor 観戦、csa_client の `live_jsonl` と組で自局の手単位リアルタイム観戦)・`--ratings` レート併記付き。[詳細](kifu_player.md)） |
| `book_from_csa` | CSA 棋譜群から YANEURAOU-DB2016 テキスト定跡 `.db` を生成。消費時間による定跡手判定（手番側ごとの即指しプレフィックス）・レート/勝敗/手数フィルタ・最小 ply 集約・ponder 集計。決定的（[詳細](book_from_csa.md)） |
| `book_kachi_label` | YANEURAOU-DB2016 テキスト定跡のノード×候補手ごとに CSA corpus から `%KACHI` 決着率を集計し、sidecar JSONL と report を出力（flip 合流対応、[詳細](book_kachi_label.md)） |

## ベンチマーク・評価

| ツール | 説明 |
|--------|------|
| `benchmark` | USI の評価ハッシュ設定・受信期限に対応。 YaneuraOu bench 互換の標準ベンチマーク。マルチスレッド対応 |
| `bench_nnue_eval` | NNUE の固定局面 eval-only と巡回局面 refresh + eval の ns/op。LayerStacks 専用モードは bucket 分布も出力（[詳細](bench_nnue_eval.md)） |
| `search_only_ab` | search-only A/B ベンチマーク。起動・ロード時間を除外して cycles/node, instructions/node を正確計測。Linux は `perf stat --control`、Windows は ETW NT Kernel Logger の PMC counting（要管理者権限、Hyper-V/VBS 共存可）。CLI 差異は `--perf-events`(Linux) ↔ `--pmc-sources`(Windows) の置き換えと、Windows での `--cpus` shard 並列未対応の 2 点。JSON レポートは `samples` / `summary` が両 OS でスキーマ互換（`cli` ブロックのみ `perf_events` / `pmc_sources` のフィールド名差があり非互換）。Windows backend は run ごとに ETW セッションを STOP→drain して末尾のスライスまで回収し、欠落を検出した run は破棄する（[詳細](search_only_ab.md)） |
| `eval_sfens` | SFEN 局面を LayerStacks NNUE で静的評価（`score` は歩=90 の内部スケール、`score_cp` は cp） |
| `nnue_saturation` | LayerStacks NNUE の活性飽和率（u8 127 張り付き）を実局面で計測（[詳細](nnue_saturation.md)） |
| `ek_testset` | held-out CSA から入玉評価テストセットを構築し、native NNUE 評価または hcpe export → yardstick で採点（[詳細](ek_testset.md)） |
| `nyugyoku_metrics` | 終局 CSA から宣言ルール距離ペア（`%KACHI`）と探索読み切り詰み距離（`%TORYO` + oracle 探索）を抽出し、native NNUE 静的評価の順序一致率 / concordance / 詰み手 top-1 率を対局クラスタ bootstrap CI 付きで採点（[詳細](nyugyoku_metrics.md)） |
| `compare_eval_nnue` | 静的評価と探索スコアを区別して NNUE を比較（期限・EOF 検査、型付き mate 保存、[詳細](compare_eval_nnue.md)） |
| `dump_effect_bucket_golden` | 形式一致 golden 用に effect bucket active index を config 別に dump（[詳細](dump_effect_bucket_golden.md)） |
| `compare_nodes` | 2つの USI エンジン間で探索ノード数を深度別に比較。エンジン別の任意ノード上限を併用可能。alignment 調査用（[詳細](compare_nodes.md)） |
| `verify_nnue_accumulator` | NNUE accumulator の refresh vs differential update 一致テスト。PSQT・Threat・LayerStacks 対応 |
| `extract_bench_positions` | floodgate CSA / selfplay JSONL から教師ラベル品質測定用のベンチ局面を抽出（層化サンプル + 入玉オーバーサンプル + 互角局面） |
| `label_bench_positions` | ベンチ局面 jsonl を深い探索（depth / nodes 指定）でラベル付けし `eval_deep` 等を追記（ground truth、局面ごと隔離で `--threads` 非依存に bit 一致） |
| `label_bench_dl` | `label_bench` jsonl の各局面を DL水匠 (標準 dlshogi ONNX) value head で静的評価し `eval_dl`（先手視点 cp）を追記（`dlshogi-onnx` feature、default 有効） |
| `yardstick_label` | ラベル品質「物差し」ステージ 1。held-out hcpe を labeler（NNUE + 固定 depth）の決定的探索でラベル付けし採点用 jsonl（手番側視点 `wdl`/`eval_ref`/`eval_label` + class）を出す |
| `yardstick_score` | ラベル品質「物差し」ステージ 2。`yardstick_label` 出力を engine ごとに勝率スケール較正し per-class の WDL logloss / 参照天井（符号一致）/ リファレンス一致（win-prob MAE・Spearman）を出す |
| `book_rescore` | YANEURAOU-DB2016 テキスト定跡の候補手に USI 探索または ONNX 静的評価値を付与し、journal/resume と集計 report を出力（実行中は進捗/ETA を stderr 表示） |
| `book_extend` | YANEURAOU-DB2016 テキスト定跡の候補集合へ USI エンジン bestmove を `count=0` で追加し、parent-journal 再利用、journal/resume、Markdown report を出力（[詳細](book_extend.md)） |
| `book_backprop` | YANEURAOU-DB2016 テキスト定跡 `.db` の候補手評価値を book 内の子局面から negamax 逆伝播し、SCC 循環と flip 合流に対応（[詳細](book_backprop.md)） |

## NNUE 学習

| ツール | 説明 |
|--------|------|
| `train_nnue` | 教師データから Adam 最適化で NNUE モデルを学習 |
| `generate_training_data` | SFEN 局面をエンジン探索で評価し、評価値付き教師データを JSONL 出力 |

## 教師データ処理

| ツール | 説明 |
|--------|------|
| `shuffle_psv` | PSV ファイル内のレコード（40バイト単位）をシャッフル。seed 指定による再現と、大規模向けチャンク方式・opt-in の段階削除に対応（[詳細](shuffle_psv.md)） |
| `split_psv` | PSV ファイルを局面数または容量で複数ファイルへ分割 |
| `merge_psv` | 複数の PSV ファイルを順序どおりストリーミング結合 |
| `relabel_psv` | PSV の score を game_result 由来値へ置換し、宣言勝ち override / diversion 整合性 deblunder / dry-run / verdict sidecar に対応（[詳細](relabel_psv.md)） |
| `rescore_psv` | PSV 評価値を NNUE / 外部エンジン / ONNX (dlshogi・AobaZero, GPU/TensorRT) で再計算。qsearch-leaf ラベル・policy 展開・レジューム・score sidecar (`--out-scores`、dlshogi ONNX / NNUE 静的評価) 対応。LayerStacks routing の bucket 数不一致は拒否 (`--allow-routing-buckets-mismatch` で明示許可)（[詳細](rescore_psv.md)） |
| `psv_gate_by_king_zone` | 入玉ドメインで base / override score を合成、または行対応 mask bitmap を生成（[詳細](psv_gate_by_king_zone.md)） |
| `psv_dual_label` | 通常 PSV の score 退避、DL score / entered sidecar と dual-label PSV の相互変換・fail-closed 検証（[詳細](psv_dual_label.md)） |
| `psv_select_by_mask` | LSB-first bitmap mask の bit 1 に対応する PSV 行を入力順にストリーミング抽出（[詳細](psv_select_by_mask.md)） |
| `psv_scatter_by_mask` | compact PSV の score を mask 対応で元 shard にストリーミング書き戻し（[詳細](psv_scatter_by_mask.md)） |
| `rescore_hcpe` | hcpe 教師の eval を NNUE 固定 depth 探索で付け替え（局面/結果は保持）。共有コア `teacher_labeler` 経由で `yardstick_label` とラベル bit 一致。fresh-per-position で分散ラベリング可、チャンク単位 + 途中（intra-chunk）resume 対応 |
| `preprocess_psv` | PSV に qsearch leaf 置換を適用し、任意で局面変更行の LSB-first bitmap mask を生成。チャンクストリーミング処理対応（[詳細](pack_tools.md#preprocess_psv)） |
| `filter_teacher_data` | 王手除外・スコアフィルタ・クリップなどの前処理を適用 |
| `fix_scores` | preprocess で上書きされたスコアを元ファイルから復元 |
| `psv_to_jsonl` | PSV 形式を JSONL 形式に変換 |
| `jsonl_to_psv` | tournament / analyze_selfplay 互換の自己対局 JSONL を PSV に変換。書き手のクラッシュで壊れた行は破棄して継続し、件数を Summary に計上（[詳細](pack_tools.md#jsonl_to_psv)） |
| `psv_to_hcpe3` | PSV を dlshogi 学習用 hcpe3 / hcpe に変換（通常手は cshogi と byte 一致、streaming、`move16=0` の有効な着手なしレコードを件数付きスキップ、`--evalfix-a` 対応） |
| `migrate_psv_move16` | 旧リポジトリ形式 (B) の PSV move16 を実 YaneuraOu 形式 (A) へストリーミング移行（[詳細](migrate_psv_move16.md)） |
| `pack_to_psv` | GenSfen .pack を PackedSfenValue (PSV) 形式に展開し、実着手の move16 を実 YaneuraOu 形式へ変換 |
| `hcpe_to_psv` | hcpe (cshogi HuffmanCodedPosAndEval) を PSV に変換（Linux/macOS・Windows対応、hardlinkを含む入力と出力の同一実体を拒否。外部公開 hcpe プールの `--data`/`--test-data` 用、[詳細](hcpe_to_psv.md)） |
| `prep_hcpe` | hcpe 教師プールの汚染除去・Bloom 重複除去・決定的 shuffle・件数制限・分割（[詳細](prep_hcpe.md)） |

## 重複除去・検証

| ツール | 説明 |
|--------|------|
| `psv_dedup` | PSV ファイルの局面重複削除（HashSet 方式、中規模向け） |
| `psv_dedup_bloom` | 大規模 PSV ファイルのブルームフィルタ重複除去（数百億レコード対応、近似） |
| `psv_dedup_partition` | ディスクパーティション方式の exact 重複除去（低メモリ・大規模向け、Phase 1 buffer 自動調整・fused shuffle 対応） |
| `psv_dedup_check` | PSV ファイルの重複率を統計出力（近似モード・正確モード対応） |
| `validate_sfens` | SFEN テキストの不正局面を検出・除去（文法・玉の存在・駒数超過・二歩など） |

## SPSA パラメータチューニング

| ツール | 説明 |
|--------|------|
| `spsa` | regex 対象外の有効な基準値も両 engine へ適用。schedule の有限性を事前検査。[SPSA チューナー](spsa_runbook.md#14-engine-プール再試行停止)。永続 engine プール、`--engine-retries` / `--nodes-timeout-ms`。初期化の最終失敗・panic は engine 破棄前に停止通知。watchdog は勝敗に使わず、stdin write 停滞は停止保証対象外。対局履歴をエンジンに送信し、千日手を自動終局。paired antithetic + stochastic rounding + 1 batch = 1 update のスケジュールで対局を回す。複数 seed の探索は `--seed` を変えた独立 run dir を別プロセスで並列実行する |
| `generate_spsa_params` | 実エンジンと共通の既定値から SPSA 用 .params ファイルを生成 |
| `generate_net_spsa_params` | LayerStacks `.bin` を走査し、net 重み delta 用 SPSA `.params` を生成（[詳細](generate_net_spsa_params.md)） |
| `apply_net_spsa_params` | 重複係数を拒否し、SPSA の net 重み delta を LayerStacks `.bin` へ焼き込み、feature 非依存の読み戻し検証と SHA-256 report を行う（[詳細](apply_net_spsa_params.md)） |
| `spsa_param_diff` | SPSA .params の最終差分と履歴差分を集計 |
| `spsa_stats_to_plot_csv` | SPSA 統計を可視化用 CSV に整形。入力 alias を拒否し成功時だけ置換（[詳細](spsa_stats_to_plot_csv.md)） |
| `params_to_shogitest_options` | SPSA .params を shogitest 互換オプション文字列に変換 |

## 外部連携・ログ解析

| ツール | 説明 |
|--------|------|
| `floodgate_pipeline` | Floodgate 棋譜の取得・変換パイプライン（CSA → SFEN → mirror → dedup、`live-mirror --push` で MONITOR2 着手通知を使う当日対局のリアルタイムミラー）。[詳細](floodgate_pipeline.md) |
| `shogitest_sprt_log_to_csv` | shogitest SPRT ログを Elo・LLR・対局結果の CSV に変換 |

## パイプライン例

```
教師データ生成 (gensfen)
  → シャッフル (shuffle_psv)
  → 前処理 (preprocess_psv)
  → 学習 (train_nnue)
  → 対局評価 (tournament → analyze_selfplay)
  → SPSA チューニング (spsa)
```
