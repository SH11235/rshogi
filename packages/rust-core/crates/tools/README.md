# ツール集（crates/tools）

このクレートには、将棋エンジン/NNUE 学習のためのユーティリティ群が含まれます。主運用は「JSONL 生成 → キャッシュ作成 → 学習 → 解析/抽出/マージ」です。

## ビルド

- ワークスペース全体: `cargo build --release`
- 本クレートの特定バイナリ: `cargo build --release --bin <ツール名>`

## 推奨ワークフロー（学習データ〜学習）

1) 教師データ生成（JSONL 推奨）
```bash
cargo run --release -p tools --bin generate_nnue_training_data -- \
  input.sfens runs/out_pass1.jsonl 2 100 \
  --engine enhanced \
  --output-format jsonl \
  --multipv 2 \
  --label wdl --wdl-scale 600 \
  --hash-mb 16
```
- 進捗/再開: `*.progress` によるレジューム対応。スキップは `*_skipped.*` に出力。
- 補足: Phase-2 では JSONL 出力と MultiPV≥2 を推奨。

2) 特徴キャッシュ作成（高速学習向け）
```bash
cargo run --release -p tools --bin build_feature_cache -- \
  -i runs/out_pass1.jsonl -o runs/out_pass1.cache \
  -l wdl --scale 600 --exclude-no-legal-move --exclude-fallback
```
- 注意: `--compress` フラグは予約済み（未実装）。

3) NNUE 学習（キャッシュ入力推奨）
```bash
# キャッシュから学習（推奨）
cargo run --release -p tools --bin train_nnue -- \
  -i runs/out_pass1.cache -e 2 -b 16384 --lr 0.001 --seed 42 -o runs/my_nnue

# JSONLからの学習（I/O負荷が高め）
cargo run --release -p tools --bin train_nnue -- \
  -i runs/out_pass1.jsonl -e 2 -b 8192 --lr 0.001 --seed 42 -o runs/my_nnue_jsonl

# 量子化モデルの保存
cargo run --release -p tools --bin train_nnue -- \
  -i runs/out_pass1.cache -e 1 --quantized -o runs/my_nnue_q
```

4) 品質解析（ゲート/要約/複数入力の比較）
```bash
cargo run --release -p tools --bin analyze_teaching_quality -- \
  runs/out_pass1.jsonl --summary --report exact-rate --report gap2 \
  --gate '{"exact_top1_min":0.98,"exact_both_min":0.90}' --gate-mode warn

# 複数入力の比較 + 重複除去
cargo run --release -p tools --bin analyze_teaching_quality -- \
  runs/p1.jsonl --inputs runs/p2.jsonl --dedup-by-sfen --summary
```
- 入力圧縮: `.gz`は標準対応。`.zst`は tools の `zstd` feature 有効時に対応（未有効時は実行時に警告）。

5) 抽出/マージ（パイプライン連携）
```bash
# 曖昧局面などの抽出（gapしきい値/非EXACT/aspiration失敗等）
cargo run --release -p tools --bin extract_flagged_positions -- \
  runs/out_pass1.jsonl runs/p2_candidates.sfens --gap-threshold 35 --include-non-exact

# JSONL の結合（SFEN重複は深さ/EXACT優先で解消）
cargo run --release -p tools --bin merge_annotation_results -- \
  runs/p1.jsonl runs/p2.jsonl runs/p3.jsonl runs/final.jsonl --dedup-by-sfen --prefer-deeper
```

## バイナリ一覧（主要）

- データ生成/学習/解析
  - `generate_nnue_training_data`: 教師データ生成（JSONL/テキスト、レジューム/スキップ、MultiPV 対応）
  - `build_feature_cache`: JSONL→特徴キャッシュ（ストリーミング）
  - `train_nnue`: NNUE 学習（行疎更新、量子化保存対応）
  - `train_wdl_baseline` / `train_cp_baseline`: 軽量ベースライン学習（WDL/CP）
  - `analyze_teaching_quality`: 品質解析/要約/ゲート/複数入力比較/近似分位
  - `extract_flagged_positions`: JSONL から条件抽出
  - `merge_annotation_results`: JSONL マージと重複解消
  - `validate_cp_dataset`: CP テキストデータの整合チェック（レガシー）

- 定跡/ユーティリティ
  - `convert_opening_book`: 定跡 SFEN → バイナリ変換
  - `search_opening_book`: バイナリ定跡のハッシュ/局面検索
  - `sfen_hasher`: SFEN → ハッシュ値
  - `debug_position`: 局面デバッグ/パーフト/ムーブ適用
  - `create_mock_nnue`: テスト用 NNUE 重み生成

- ベンチマーク
  - `parallel_benchmark`: 並列探索ベンチ（NPS/重複/効率）
  - `nnue_benchmark`: 評価/探索のNNUE対比ベンチ
  - `simd_benchmark` / `simd_check`: SIMD の効果測定/機能確認
  - `quick_perf_test`: TT/探索の簡易パフォーマンステスト
  - `metrics_analyzer`: エンジンログ（`kind=bestmove_*`）の集計

## 追加ドキュメント

- 教師データ生成ガイド: `docs/nnue-training-data-generation.md`
- 機能要件タスク一覧: `../../docs/tasks/nnue-training-functional-tasks.md`

### ゲート設定サンプル
- サンプルファイル: `crates/tools/ci_gate.sample.json`
- 使用例:
```bash
cargo run --release -p tools --bin analyze_teaching_quality -- \
  runs/final.jsonl --summary --gate crates/tools/ci_gate.sample.json --gate-mode fail
```

## 注意事項

- `build_feature_cache --compress` は未実装（将来対応予定）。
- `.zst` 入力は `analyze_teaching_quality` の feature 有効時のみ対応（既定は無効）。
- 旧テキスト系スクリプト・CP専用ワークフローはレガシー扱い（ベースライン検証用途に限り維持）。
