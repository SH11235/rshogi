# tools

将棋エンジン開発用ツール群

## ツール一覧

### 対局・データ生成

| ツール | 説明 |
|--------|------|
| `tournament` | 複数エンジンの round-robin 並列トーナメント、SPRT 検定 |
| `analyze_selfplay` | tournament 出力の集計・Elo/nElo 算出・SPRT post-hoc 判定（[詳細](docs/analyze_selfplay.md)） |
| `floodgate_record` | csa_client の per-game JSONL から 1 エンジンの戦績を集計（先後別勝率・相手別・後手勝ち/負け/引分・実戦 NPS、`--config` で csa_client 設定から入力導出、`--fetch-ratings` で wdoor 現在レート併記・履歴記録。floodgate 連続対局向け、[詳細](docs/floodgate_record.md)） |
| `gensfen` | NNUE 学習用 PSV/pack/hcpe3 教師局面の生成（PSV move16 は実 YaneuraOu 形式、hcpe3 policy は既定 65535 票・温度 100、`--hcpe3-eval-drop-threshold` による候補除外と終局理由/gameInfo 符号化、USI engine vs engine／NativeBackend、native LS progress 係数、千日手裁定、異常終局の全局破棄、宣言勝ち PSV 終端局面、乱択来歴 JSONL 記録 (--omit-diversions で件数のみに省略可、deblunder 非互換)、FV_SCALE override、control.json 動的制御・drain、Windows でも動作可 (親 dir fsync はスキップされ電源断耐性が Unix より弱い)） |
| `floodgate_pipeline` | Floodgate棋譜のダウンロード・変換・`live-mirror --push` MONITOR2 着手通知付きリアルタイムミラー（[詳細](docs/floodgate_pipeline.md)） |
| `nyugyoku_gensfen` | CSA manifest から入玉アンカー局面を disk-partition exact dedup で抽出し、checkpoint/resume 付きで gensfen 用 `startpos.txt` と出典 TSV を生成（[詳細](docs/nyugyoku_gensfen.md)） |
| `book_from_csa` | CSA 棋譜群から YANEURAOU-DB2016 テキスト定跡 `.db` を生成（消費時間による定跡手判定・レート/勝敗フィルタ、[詳細](docs/book_from_csa.md)） |
| `book_kachi_label` | YANEURAOU-DB2016 テキスト定跡のノード×候補手ごとに CSA corpus から `%KACHI` 決着率を集計し sidecar JSONL を生成（flip 合流対応、[詳細](docs/book_kachi_label.md)） |
| `book_rescore` | YANEURAOU-DB2016 テキスト定跡の候補手に USI 探索または ONNX 静的評価値を付与、実行中は進捗/ETA を stderr 表示（[詳細](docs/book_rescore.md)） |
| `book_extend` | YANEURAOU-DB2016 テキスト定跡の候補集合へ USI エンジン bestmove を `count=0` で追加（[詳細](docs/book_extend.md)） |
| `book_backprop` | YANEURAOU-DB2016 テキスト定跡 `.db` の候補手評価値を book 内の子局面から negamax 逆伝播（[詳細](docs/book_backprop.md)） |

### 棋譜閲覧

| ツール | 説明 |
|--------|------|
| `kifu_player` | PSV / tournament JSONL / CSA を同じ TUI で再生・閲覧（評価値グラフ・検索/絞り込み（SFEN 局面検索含む）・`--live` 追記監視 (live-mirror と組で wdoor 観戦、csa_client の `live_jsonl` と組で自局の手単位リアルタイム観戦)・`--ratings` レート併記付き。[詳細](docs/kifu_player.md)） |

### 学習データ処理

| ツール | 説明 |
|--------|------|
| `shuffle_psv` | PSV ファイルのシャッフル |
| `split_psv` | PSV ファイルを局面数または容量で分割 |
| `merge_psv` | 複数の PSV ファイルを順序どおり結合 |
| `relabel_psv` | PSV の score を game_result 由来値へ置換し、宣言勝ち override / diversion 整合性 deblunder / dry-run / verdict sidecar に対応（[詳細](docs/relabel_psv.md)） |
| `rescore_psv` | 局面の再評価（探索スコア付与） |
| `psv_gate_by_king_zone` | 入玉ドメインの score 合成 / mask bitmap 生成 |
| `rescore_hcpe` | hcpe 教師の eval を NNUE 固定 depth 探索で付け替え（分散ラベリング・チャンク単位 + 途中 resume 対応） |
| `preprocess_psv` | PSV ファイルの前処理（qsearch leaf置換等） |
| `validate_psv` | PSV ファイルの不正局面検出・除去 |
| `psv_to_jsonl` | PSV 形式 → JSONL 変換（デバッグ・確認用） |
| `jsonl_to_psv` | 自己対局 JSONL → PSV 変換（破損行は破棄して継続、件数を Summary に計上。[詳細](docs/pack_tools.md#jsonl_to_psv)） |
| `psv_to_hcpe3` | PSV → dlshogi 学習用 hcpe3 / hcpe 変換（cshogi 互換、streaming、`move16=0` の有効な着手なしレコードを件数付きスキップ、`--evalfix-a` 対応） |
| `migrate_psv_move16` | 旧リポジトリ形式 (B) の PSV move16 を実 YaneuraOu 形式 (A) へ移行（[詳細](docs/migrate_psv_move16.md)） |
| `pack_to_psv` | GenSfen .pack → PSV 変換（move16 は実着手ラベルの実 YaneuraOu 形式） |
| `fix_scores` | スコアの補正 |
| `psv_dedup` / `psv_dedup_bloom` / `psv_dedup_partition` | PSV 局面の重複除去（3 方式。使い分けは [pack_tools.md](docs/pack_tools.md#重複除去ツールの選び方)） |
| `prep_hcpe` | hcpe 教師プールの汚染除去・重複除去・決定的 shuffle・分割（[詳細](docs/prep_hcpe.md)） |
| `hcpe_to_psv` | hcpe → PSV 変換（Linux/macOS・Windows対応、hardlinkを含む入力と出力の同一実体を拒否。外部公開 hcpe プールの学習/検証投入用、[詳細](docs/hcpe_to_psv.md)） |

### ベンチマーク・分析

| ツール | 説明 |
|--------|------|
| `benchmark` | エンジン性能ベンチマーク |
| `compare_nodes` | 2つの USI エンジン間で探索ノード数を深度別に比較。エンジン別の任意ノード上限を併用可能（[詳細](docs/compare_nodes.md)） |
| `compare_eval_nnue` | NNUE評価値の比較 |
| `dump_effect_bucket_golden` | 形式一致 golden 用に effect bucket active index を config 別に dump（[詳細](docs/dump_effect_bucket_golden.md)） |
| `extract_bench_positions` | floodgate CSA / selfplay JSONL から教師ラベル品質測定用のベンチ局面を抽出 |
| `label_bench_positions` | ベンチ局面 jsonl を深い探索でラベル付けし `eval_deep` を追記（ground truth） |
| `label_bench_dl` | `label_bench` jsonl の各局面を DL水匠 (標準 dlshogi ONNX) で静的評価し `eval_dl` を追記（`dlshogi-onnx` feature、default 有効） |
| `yardstick_label` / `yardstick_score` | ラベル品質「物差し」。held-out hcpe を labeler でラベル付け（stage 1）→ engine ごとに勝率較正して per-class WDL logloss / 参照天井 / リファレンス一致を採点（stage 2） |
| `ek_testset` | held-out CSA から入玉評価テストセットを構築し、native NNUE 評価で DT/OC 指標を採点（[詳細](docs/ek_testset.md)） |
| `nyugyoku_metrics` | 終局 CSA から宣言ルール距離ペア（`%KACHI`）と探索読み切り詰み距離（`%TORYO` + oracle 探索）を抽出し、native NNUE 静的評価の順序一致率 / concordance / 詰み手 top-1 率を採点（[詳細](docs/nyugyoku_metrics.md)） |
| `nnue_saturation` | LayerStacks NNUE の活性飽和率（ClippedReLU 127 張り付き）を実局面で計測（[詳細](docs/nnue_saturation.md)） |

### NNUE 学習

NNUE モデルの学習には [bullet-shogi](https://github.com/SH11235/bullet-shogi/tree/shogi-support) を使用しています。
教師データは上記の PSV ツール群で生成・前処理し、bullet-shogi で学習を行います。

## クイックスタート

### 教師局面の生成（gensfen）

```bash
cargo run -p tools --release --bin gensfen -- \
  --games 100 --byoyomi 1000
# → runs/gensfen/<timestamp>/gensfen.psv
```

### 学習データのシャッフル

```bash
cargo run -p tools --release --bin shuffle_psv -- \
  --input data.psv --output shuffled.psv
```

### ベンチマーク実行

```bash
cargo run -p tools --release --bin benchmark -- --internal
```

## ドキュメント

各ツールの詳細は `docs/` を参照：

- [tournament](docs/tournament.md) - 並列トーナメント・SPRT 検定
- [kifu_player](docs/kifu_player.md) - PSV / tournament JSONL / CSA 共通の棋譜プレイヤー TUI（評価値グラフ・検索/絞り込み（SFEN 局面検索含む）・`--live` 追記監視 (live-mirror と組で wdoor 観戦、csa_client の `live_jsonl` と組で自局の手単位リアルタイム観戦)・`--ratings` レート併記付き）
- [gensfen](docs/gensfen.md) - 教師局面生成ツールの詳細
- [nyugyoku_gensfen](docs/nyugyoku_gensfen.md) - 入玉アンカー局面から gensfen 用開始局面を生成
- [benchmark](docs/benchmark.md) - ベンチマークツールの詳細
- [dump_effect_bucket_golden](docs/dump_effect_bucket_golden.md) - 形式一致 golden 用 effect bucket active index dump
- [pack_tools](docs/pack_tools.md) - 学習データ処理ツール群
- [extract_bench_positions](docs/extract_bench_positions.md) - 教師ラベル品質測定用ベンチ局面の抽出
- [label_bench_positions](docs/label_bench_positions.md) - ベンチ局面の深い探索ラベリング（ground truth）
- [label_bench_dl](docs/label_bench_dl.md) - label_bench jsonl への DL水匠 (dlshogi ONNX) 評価値追記
- [yardstick_label](docs/yardstick_label.md) - held-out hcpe を labeler の固定 depth 探索でラベル付け（物差し stage 1）
- [yardstick_score](docs/yardstick_score.md) - labeler の WDL logloss / 参照天井 / リファレンス一致を採点（物差し stage 2）
- [ek_testset](docs/ek_testset.md) - held-out CSA から入玉評価テストセットを構築し、native NNUE 評価で DT/OC 指標を採点
- [nyugyoku_metrics](docs/nyugyoku_metrics.md) - 終局 CSA から宣言ルール距離ペアと探索読み切り詰み距離を抽出し、NNUE 静的評価の順序一致率 / concordance / 詰み手 top-1 率を採点
- [nnue_saturation](docs/nnue_saturation.md) - LayerStacks NNUE の活性飽和率（u8 127 張り付き）を実局面で計測
- [rescore_psv](docs/rescore_psv.md) - PSV 評価値の再スコアリング（推奨: dlshogi ONNX + TensorRT FP16。qsearch-leaf ラベル / policy 展開 / レジューム対応）
- [psv_gate_by_king_zone](docs/psv_gate_by_king_zone.md) - 入玉ドメインの depth9/DL 合成と行対応 mask bitmap
- [relabel_psv](docs/relabel_psv.md) - PSV score の勝敗ラベル置換、宣言勝ち override、diversion 整合性 deblunder
- [rescore_hcpe](docs/rescore_hcpe.md) - hcpe 教師の eval を NNUE 固定 depth 探索で付け替え（共有コアで yardstick とラベル bit 一致、分散ラベリング・チャンク単位 + 途中 resume 対応）
- [psv_to_hcpe3](docs/psv_to_hcpe3.md) - PSV → dlshogi 学習用 hcpe3 / hcpe 変換（cshogi 互換、streaming、`move16=0` の有効な着手なしレコードを件数付きスキップ、`--evalfix-a` 対応）
- [migrate_psv_move16](docs/migrate_psv_move16.md) - 旧 PSV move16 の実 YaneuraOu 形式への移行
- [book_rescore](docs/book_rescore.md) - YANEURAOU-DB2016 テキスト定跡の候補手に USI 探索または ONNX 静的評価値を付与（実行中は進捗/ETA を stderr 表示）
- [book_kachi_label](docs/book_kachi_label.md) - YANEURAOU-DB2016 テキスト定跡の候補手に `%KACHI` 決着率を付与する sidecar JSONL を生成
- [book_extend](docs/book_extend.md) - YANEURAOU-DB2016 テキスト定跡の候補集合へ USI エンジン bestmove を `count=0` で追加
- [book_backprop](docs/book_backprop.md) - YANEURAOU-DB2016 テキスト定跡 `.db` の候補手評価値を book 内の子局面から negamax 逆伝播

各ツールのオプション一覧は `--help` で確認できます。

## 使用例

より多くのコマンド例は [examples/README.md](examples/README.md) を参照。
