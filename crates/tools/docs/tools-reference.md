# rshogi tools リファレンス

crates/tools/src/bin/ 配下の全31バイナリの一覧と解説。

## 対局・トーナメント

| ツール | 説明 |
|--------|------|
| `tournament` | 複数エンジンの round-robin 並列トーナメント。JSONL 出力 |
| `engine_selfplay` | 2エンジン間の自己対局ハーネス。学習データ (PSV) 出力対応 |
| `csa_client` | USI エンジンを floodgate 等の CSA サーバーに接続して連続対局 |
| `analyze_selfplay` | 自己対局の JSONL ログを集計。勝率・Elo 差・NPS 等を表示 |

## ベンチマーク・評価

| ツール | 説明 |
|--------|------|
| `benchmark` | YaneuraOu bench 互換の標準ベンチマーク。マルチスレッド対応 |
| `bench_nnue_eval` | NNUE 推論単体の性能測定（cycles/eval, instructions/eval） |
| `search_only_ab` | Linux perf ベースの search-only A/B ベンチマーク。起動・ロード時間を除外して正確計測 |
| `eval_sfens` | SFEN 局面を LayerStacks NNUE で静的評価 |
| `compare_eval_nnue` | 教師 NNUE と生徒 NNUE の評価値一致度を検証（MAE・相関係数・スコア帯別誤差） |
| `compare_nodes` | 2つの USI エンジン間で探索ノード数を深度別に比較。alignment 調査用 |
| `verify_nnue_accumulator` | NNUE accumulator の refresh vs differential update 一致テスト。PSQT・Threat・LayerStacks 対応 |

## NNUE 学習

| ツール | 説明 |
|--------|------|
| `train_nnue` | 教師データから Adam 最適化で NNUE モデルを学習 |
| `generate_training_data` | SFEN 局面をエンジン探索で評価し、評価値付き教師データを JSONL 出力 |

## 教師データ処理

| ツール | 説明 |
|--------|------|
| `shuffle_pack` | pack ファイル内のレコード（40バイト単位）をシャッフル |
| `rescore_pack` | pack ファイルの評価値を NNUE または外部エンジンで再計算 |
| `preprocess_pack` | pack ファイルに qsearch leaf 置換を適用 |
| `filter_teacher_data` | 王手除外・スコアフィルタ・クリップなどの前処理を適用 |
| `fix_scores` | preprocess で上書きされたスコアを元ファイルから復元 |
| `pack_to_jsonl` | YaneuraOu pack 形式を JSONL 形式に変換 |
| `pack_to_psv` | GenSfen .pack を PackedSfenValue 形式に展開 |

## 重複除去・検証

| ツール | 説明 |
|--------|------|
| `psv_dedup` | PSV ファイルの局面重複削除（HashSet 方式） |
| `psv_dedup_bloom` | 大規模 PSV ファイルのブルームフィルタ重複除去（数百億レコード対応） |
| `psv_dedup_check` | PSV ファイルの重複率を統計出力（近似モード・正確モード対応） |
| `validate_sfens` | SFEN テキストの不正局面を検出・除去（文法・玉の存在・駒数超過・二歩など） |

## SPSA パラメータチューニング

| ツール | 説明 |
|--------|------|
| `spsa` | Fishtest 互換の並列 SPSA チューナー。seed 多重対応 |
| `generate_spsa_params` | SearchTuneParams から SPSA 用 .params ファイルを生成 |
| `spsa_param_diff` | SPSA .params の最終差分と履歴差分を集計 |
| `spsa_stats_to_plot_csv` | SPSA 統計を可視化用 CSV に整形（移動平均計算） |
| `params_to_shogitest_options` | SPSA .params を shogitest 互換オプション文字列に変換 |

## 外部連携・ログ解析

| ツール | 説明 |
|--------|------|
| `floodgate_pipeline` | Floodgate 棋譜の取得・変換パイプライン（CSA → SFEN → mirror → dedup） |
| `shogitest_sprt_log_to_csv` | shogitest SPRT ログを Elo・LLR・対局結果の CSV に変換 |

## パイプライン例

```
教師データ生成 (engine_selfplay)
  → シャッフル (shuffle_pack)
  → 前処理 (preprocess_pack)
  → 学習 (train_nnue)
  → 対局評価 (tournament → analyze_selfplay)
  → SPSA チューニング (spsa)
```
