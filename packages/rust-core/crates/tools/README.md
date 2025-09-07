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

# 圧縮付き（ペイロードのみ圧縮。ヘッダは非圧縮）
cargo run --release -p tools --bin build_feature_cache -- \
  -i runs/out_pass1.jsonl -o runs/out_pass1.cache \
  -l wdl --scale 600 --compress --compressor gz --compress-level 6
# zstd を使う場合（tools クレートを `--features zstd` でビルド）
cargo run --release -p tools --bin build_feature_cache --features zstd -- \
  -i runs/out_pass1.jsonl -o runs/out_pass1.cache -l wdl --compress --compressor zst --compress-level 10
```
 
  - v1 フォーマット（既定）では 2 サンプル/局面（先手視点・後手視点）に分割し、ラベルは黒基準で整合化します。
  - `--dedup-features` で特徴の重複を除去（デフォルトOFF）。統計に dedup の有無を表示。
  - 圧縮はヘッダ非圧縮＋ペイロード部のみ圧縮（gzip/zstd）。トレーナはヘッダの `payload_encoding` を自動判別して読込。

3) NNUE 学習（キャッシュ入力推奨）
```bash
# キャッシュから学習（推奨。v1/圧縮対応）
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
# 注: `--gap-threshold` / `--max-gap-cp` は「以下を拾う」条件（best2_gap_cp <= しきい値）。
cargo run --release -p tools --bin extract_flagged_positions -- \
  runs/out_pass1.jsonl runs/p2_candidates.sfens --gap-threshold 35 --include-non-exact

# しきい値の別名（同義）: --max-gap-cp
cargo run --release -p tools --bin extract_flagged_positions -- \
  runs/out_pass1.jsonl - --max-gap-cp 20 | head

# STDIN/STDOUT による抽出
cat runs/out_pass1.jsonl | cargo run --release -p tools --bin extract_flagged_positions -- - - --gap-threshold 35

# JSONL の結合（SFEN重複は安定タイブレークで解消）
cargo run --release -p tools --bin merge_annotation_results -- \
  runs/p1.jsonl runs/p2.jsonl runs/p3.jsonl runs/final.jsonl --dedup-by-sfen

# 深さ優先（既定は EXACT 優先）
cargo run --release -p tools --bin merge_annotation_results -- \
  runs/p1.jsonl runs/p2.jsonl runs/p3.jsonl runs/final.jsonl --dedup-by-sfen --prefer-deeper

# モード指定（--prefer-deeper の明示的な別名）
cargo run --release -p tools --bin merge_annotation_results -- \
  runs/p1.jsonl runs/p2.jsonl runs/p3.jsonl runs/final.jsonl --dedup-by-sfen --mode depth-first

# STDIN/STDOUT を使った結合
cat runs/p1.jsonl | cargo run --release -p tools --bin merge_annotation_results -- \
  - - --dedup-by-sfen

# STDOUT へ出力しつつ manifest の出力先を明示
cargo run --release -p tools --bin merge_annotation_results -- \
  runs/p1.jsonl runs/p2.jsonl - --dedup-by-sfen --manifest-out runs/merge_manifest.json
```

## バイナリ一覧（主要）

- データ生成/学習/解析
  - `generate_nnue_training_data`: 教師データ生成（JSONL/テキスト、レジューム/スキップ、MultiPV 対応）
  - `build_feature_cache`: JSONL→特徴キャッシュ（ストリーミング）
  - `train_nnue`: NNUE 学習（行疎更新、量子化保存対応）
  - `train_wdl_baseline` / `train_cp_baseline`: 軽量ベースライン学習（WDL/CP）
  - `analyze_teaching_quality`: 品質解析/要約/ゲート/複数入力比較/近似分位
    - `--dedup-by-sfen` の選定規則（固定）: `depth → seldepth → EXACT度 → nodes → time_ms`。完全同点では、先に現れた行（小さい `file_idx/line_idx`）を優先。
    - `--limit` と併用時は、上位（良い順）から取り込みます。完全同点のケースでは「先勝ち」のため、入力順（file/lineの小さい方）が優先されます。
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
- 次タスク計画（本書）: `../../docs/tasks/nnue-training-pipeline-v3.md`

### ゲート設定サンプル
- サンプルファイル: `crates/tools/ci_gate.sample.json`
- 使用例:
```bash
cargo run --release -p tools --bin analyze_teaching_quality -- \
  runs/final.jsonl --summary --gate crates/tools/ci_gate.sample.json --gate-mode fail
```

## 注意事項

- `build_feature_cache --compressor zst` を使用する場合は `tools` クレートを `--features zstd` でビルドしてください。
- `.gz` 入力は標準対応、`.zst` 入力は `--features zstd` 有効時に対応（`analyze_teaching_quality`/`merge_annotation_results`/`extract_flagged_positions`）。
- 旧テキスト系スクリプト・CP専用ワークフローはレガシー扱い（ベースライン検証用途に限り維持）。

### Manifest v2（生成ツール）
- `manifest_version: "2"` を採用。主なプロビナンス:
  - `teacher_engine { name, version, commit, usi_opts{hash_mb,multipv,threads,teacher_profile,min_depth} }`
  - `generation_command`, `seed`（`argv[1..]` を SHA-256 で安定生成）
- `input { path, sha256, bytes }`, `nnue_weights_sha256`（重み使用時）
- 注意: `generation_command` は CLI 引数全体を含みます。機微情報は引数では渡さない運用を推奨します。
 - `count` は全体の累計件数、`count_in_part` は当該 part のみの件数です（part出力時）。

### train_nnue の入力フィルタ（JSONL時）
- `--exclude-no-legal-move`: 合法手なしの局面を除外
- `--exclude-fallback`: 探索でフォールバックが発生した局面を除外
- これらはキャッシュビルダー（build_feature_cache）の `--exclude-*` と同等です。キャッシュ入力ではビルド時のフィルタ結果が反映されます。

### merge_annotation_results の仕様補足
- 入力圧縮: `.jsonl`, `.jsonl.gz`（標準）、`.jsonl.zst`（`--features zstd`）。
- 非 dedup: 入力をそのまま連結し、順序を保持（JSON の最小検証あり）。
- dedup の選定規則（既定 = EXACT 優先）:
  - EXACT 優先: `EXACT度 → depth → seldepth → nodes → time_ms → file_idx → line_idx`
  - 深さ優先（`--prefer-deeper`）: `depth → seldepth → EXACT度 → nodes → time_ms → file_idx → line_idx`
  - 完全同点では、先に現れた行（小さい `file_idx/line_idx`）を保持。
  - 注: `nodes` / `time_ms` は「大きいほど優先」します（より計算資源を掛けた結果を採用）。
- 出力順の安定化: dedup 時の最終出力は `sfen` 昇順で書き出し。
- STDIN/STDOUT: 入力に `-` を指定すると STDIN を読む。出力に `-` を指定すると STDOUT に書き出し。
- マニフェスト統合: 既定では出力ファイルと同ディレクトリに `manifest.json` を生成。出力が STDOUT の場合は
  `--manifest-out <path>` で出力先を明示可能（未指定時は警告を出しスキップ）。
- マニフェスト内容: `mode/inputs/sources` に加え、行カウントの詳細（`read_lines`/`valid_json_lines`/`written_lines`、互換のため `total_positions` も維持）、
  統計（min/max/avg: depth/seldepth/nodes/time_ms）、設定整合性（multipv/teacher_profile/hash_mb の一致 or varies）、
  `generated_at_range` は ISO-8601 を安全に比較（タイムゾーン付き文字列は UTC に正規化、パース不能時は文字列比較にフォールバック）。

### extract_flagged_positions の仕様補足
- 入出力: 入力に `-` を指定すると STDIN を読む。出力未指定または `-` 指定で STDOUT に書き出し。
- 入力圧縮: `.jsonl`, `.jsonl.gz`（標準）、`.jsonl.zst`（`--features zstd`）。

### cross‑dedup（train/valid/test の漏洩チェック）
- ツール: `check_cross_dedup`
- 使い方:
  ```bash
  cargo run -p tools --bin check_cross_dedup -- \
    --train runs/train.jsonl --valid runs/valid.jsonl --test runs/test.jsonl \
    --report runs/leak_report.csv
  ```
- 重複（SFEN key の一致）が見つかると `leak_report.csv` に出力し、非ゼロ終了（CI で赤）。
