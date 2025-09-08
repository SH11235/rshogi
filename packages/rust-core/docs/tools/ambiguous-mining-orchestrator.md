# 曖昧掘りオーケストレーション（orchestrate_ambiguous）

`orchestrate_ambiguous` は、初回注釈（pass1）から曖昧局面を抽出し、強設定で再注釈（K=3/entropy）して最終マージまでを 1 コマンドで実行するツールです。マージは常に depth-first を明示し、系譜（provenance）と整合性を orchestration manifest に記録します。

## 概要
- 入力: pass1 の JSONL（複数可、.gz/.zst 対応）
- 処理: 抽出（gap, 非Exact など）→ 正規化・ユニーク化 → 再注釈（generate）→ マージ
- 出力: 最終 JSONL と最終 manifest（aggregated）、オーケストレーション manifest

## すぐ使う
```bash
cargo run --release -p tools --bin orchestrate_ambiguous -- \
  --pass1 runs/out_pass1.jsonl \
  --final runs/final.jsonl \
  --gap-threshold 35 \
  --engine enhanced --teacher-profile balanced --multipv 3 --hash-mb 64 \
  --split 200000 --compress gz
```
- ドライラン（実行計画のみ表示）
```bash
cargo run --release -p tools --bin orchestrate_ambiguous -- \
  --pass1 runs/out_pass1.jsonl --final runs/final.jsonl --dry-run
```

ドライラン出力は、空白や `"` を含むパスも引用され、コピペ実行可能です（PowerShell / cmd いずれでも動作）。出力例:
```text
[dry-run] "/path/to/target/debug/extract_flagged_positions" "runs/out_pass1.jsonl" - --gap-threshold 35
[dry-run] normalize+unique -> ".final.ambdig/pass2_input.sfens"
[dry-run] "/path/to/target/debug/generate_nnue_training_data" ".final.ambdig/pass2_input.sfens" ".final.ambdig/pass2.jsonl" --engine enhanced --output-format jsonl --hash-mb 64 --multipv 3 --teacher-profile balanced --split 200000 --compress gz
[dry-run] "/path/to/target/debug/merge_annotation_results" --dedup-by-sfen --mode depth-first --manifest-out "runs/final.manifest.json" "runs/out_pass1.jsonl" ".final.ambdig/pass2.jsonl" "runs/final.jsonl"
[dry-run] "/path/to/target/debug/analyze_teaching_quality" "runs/final.jsonl" --json --expected-multipv 3 --manifest-autoload-mode strict > ".final.ambdig/quality.json"
[dry-run] would write orchestration manifest to ".final.ambdig/orchestrate_ambiguous.manifest.json"
// prune 指定時は削除計画も表示（例）
[dry-run] prune plan: 7 files, total 1234567 bytes under ".final.ambdig"
// --verbose なら個別 rm 行も出力
```

## 主なオプション
- 入出力
  - `--pass1 <FILE>`（複数可）: 初回注釈の JSONL
  - `--final <FILE>`: マージ後の最終 JSONL
  - `--out-dir <DIR>`: 中間物の保存先（既定: `<final>` と同階層に `.<stem>.ambdig/`）
  - `--manifest-out <FILE>`: オーケストレーション manifest の保存先（既定: `<out-dir>/orchestrate_ambiguous.manifest.json`）
  - `--final-manifest-out <FILE>`: 最終マージ manifest の保存先（未指定時は `<final>.manifest.json`）。
    - `--final` が `final.jsonl.gz` や `final.jsonl.zst` でも自動で `final.manifest.json` を採用。
- 抽出（extract_flagged_positions）
  - `--gap-threshold <cp>`（既定35）／`--include-non-exact`／`--include-aspiration-failures <N>`／`--include-mate-boundary`
- 再注釈（generate_nnue_training_data）
  - `--engine`／`--nnue-weights`／`--teacher-profile`（既定 balanced。orchestrator から generate に委譲）
  - `--multipv <k>`（既定3）／`--min-depth <d>`（既定は pass1 の `effective_min_depth + 1` 推定）
  - `--nodes <N>` または `--time-limit-ms <ms>`／`--jobs`／`--hash-mb`／`--reuse-tt`
  - `--split <N>`／`--compress {gz|zst}`（zstは `--features zstd`）
  - 曖昧/entropy: `--amb-gap2-threshold`／`--amb-allow-inexact`／`--entropy-mate-mode`／`--entropy-scale`
- マージ（merge_annotation_results）
  - `--merge-mode depth-first`（常に明示）／`--dedup-by-sfen`（常に有効）
- 要約
  - `--analyze-summary`（JSON は `quality.json` に保存、サマリはコンソールに出力。`--expected-multipv` は最終 manifest の aggregated.multipv → pass2 manifest → CLI の順で推定）
- 実行制御
  - `--dry-run`（extract/normalize/generate/merge/analyze の全コマンド計画を表示。空白や `"` を含むパスは引用され、コピペ実行可能）／`--verbose`／`--keep-intermediate`（既定ON）／`--prune`（常に中間物削除）／`--prune-on-success`（成功時のみ削除）
 - 正規化
   - `--normalize-sort-unique`（外部ソート＋uniqで省メモリ化）／`--normalize-chunk-lines <N>`（既定 200k 行）

## 推奨設定
- 抽出：`--gap-threshold 35`（広めに拾う）
- 再注釈：`--multipv 3`, `--teacher-profile balanced`、`--min-depth` は pass1+1 を既定
- マージ：`--merge-mode depth-first`（オーケストレータが常に明示）

## 閾値の考え方（gap と gap2）
- `--gap-threshold`（抽出）
  - 目的: pass1 から「再注釈候補」を広めに拾う粗いフィルタ。
  - 推奨: “やや広め”から開始（例: 30–50cp）。再注釈コストと相談して調整。
  - 基準: `analyze_teaching_quality --summary` の分布（中央値/下位5%など）を見て決めると再現しやすい。
- `--amb-gap2-threshold`（再注釈時の曖昧度判定）
  - 目的: K=3 再注釈で「曖昧（bestと2位が近い）」を厳密に測るための gap2 閾値。
  - 推奨: K の設定（例: 3）に合わせ、抽出よりもやや狭め（例: 15–35cp）で運用開始。
  - 備考: 抽出で広めに拾い、再注釈でより厳しく選別するのが基本方針。

ヒント: まずは `--gap-threshold` を広めに設定して母集団を確保し、`--amb-gap2-threshold` で精度と件数のバランスを取ると効果的です。

## オーケストレーション manifest
`<out-dir>/orchestrate_ambiguous.manifest.json` に、系譜・オプション・要約を記録します。
- `inputs[]`: pass1 入力と manifest 自動解決（B案）の結果
- `extract`: 抽出条件と `pass2_input.sfens` の `sha256/bytes`、抽出件数
- `reannotate`: generate のコマンドオプション、検出した part/aggregate manifest、生成件数
- `merge`: マージモード、入力一覧、`final` のパス、`final_written`
- `counts`: `pass1_total` / `extracted` / `pass2_generated` / `final_written`（`pass1_total_by_source` も付与）
- `analyze`（任意）: `quality.json` の参照と `expected_mpv` を記録

整合チェック（期待関係）
- `final.manifest.aggregated.written_lines == counts.final_written`
- `pass1_total >= extracted >= pass2_generated >= final_written`（成立しない場合は警告）

### 例（抜粋）
```json
{
  "tool": "orchestrate_ambiguous",
  "generated_at": "2025-09-08T12:34:56Z",
  "inputs": [
    {"path": "runs/p1.jsonl", "resolved_manifest_verified": true}
  ],
  "extract": {
    "opts": {"gap_threshold": 35},
    "sfens": {"path": ".final.ambdig/pass2_input.sfens", "sha256": "...", "bytes": 12345},
    "extracted_count": 1024
  },
  "reannotate": {
    "base": ".final.ambdig/pass2.jsonl",
    "outputs": [
      ".final.ambdig/pass2.part-0001.jsonl.gz",
      ".final.ambdig/pass2.part-0002.jsonl.gz"
    ],
    "opts": {"engine": "enhanced", "multipv": 3, "min_depth": 3, "hash_mb": 64},
    "pass2_generated": 1000
  },
  "merge": {
    "mode": "depth-first",
    "final": "runs/final.jsonl",
    "manifest_out": "runs/final.manifest.json",
    "final_written": 980
  },
  "counts": {"pass1_total": 1500, "extracted": 1024, "pass2_generated": 1000, "final_written": 980},
"analyze": {"summary_json": ".final.ambdig/quality.json", "expected_mpv": 3}
}
```

## out-dir の構成と処理フロー
- 典型的な out-dir 配下（`.<final-stem>.ambdig/`）のファイル:
  - `pass2_input.tmp` / `pass2_input.sfens`（抽出→正規化・重複排除の入力/出力）
  - `pass2.jsonl`（単一出力の場合のベース）
  - `pass2.part-0001.jsonl.gz`（分割出力の各 part、拡張子は `gz|zst|jsonl`）
  - `*.manifest.json`（aggregate と各 part の manifest）
  - `quality.json`（`--analyze-summary` 指定時の要約 JSON）
  - `orchestrate_ambiguous.manifest.json`（このツールの系譜・整合サマリ）

フロー（概略）:
```
pass1(.jsonl[.gz|.zst]) --extract--> pass2_input.tmp --normalize+unique--> pass2_input.sfens
      \                                                                  |
       \-- manifest 解決（件数等）                                       v
         + provenance 記録                          generate_nnue_training_data --> pass2.jsonl / pass2.part-*.jsonl.* (+ manifest)
                                                                  |
                                                                  v
                                     merge_annotation_results --depth-first--> final.jsonl (+ final.manifest.json)
                                                                  |
                                                                  v
                                      analyze_teaching_quality --summary/json--> quality.json
```

## 複数 pass1 のマージ例と優先順
- CLI 例:
```bash
cargo run --release -p tools --bin orchestrate_ambiguous -- \
  --pass1 runs/p1a.jsonl --pass1 runs/p1b.jsonl \
  --final runs/final.jsonl --merge-mode depth-first --dry-run
```
- 優先順（depth-first + dedup-by-sfen）:
  1) `pass1` を指定順で適用（先に現れたファイルが優先）
  2) その後に `pass2` の結果を適用
- 注意: `--pass1` の並び順で同一 SFEN の採用元が変わる場合があります。再現性のため、順序を固定して運用してください。

## トラブルシュート
- 抽出 0 件
  - 正常動作です。再注釈/マージはスキップされ、orchestration manifest のみ出力されます。
  - 解析を有効にしている場合、入力は先頭の pass1 に自動フォールバックし、その旨を `[info]` ログに出力します。
- `--compress zst` で失敗
  - `tools` クレートを `--features zstd` でビルドしてください。
- 解析（`--analyze-summary`）が失敗
  - 解析コマンドが非0終了でも出力がある場合は `quality.json` を保存します。出力が空の場合のみスキップします。

## メモリに関する注意
- 大規模な SFEN 入力では、`--normalize-sort-unique` を用いると on-disk の外部ソート＋uniq でメモリ使用を抑えられます（I/O は増加）。

## 関連
- 設計ドキュメント: `docs/tasks/orchestrate_ambiguous_plan.md`
- 生成ツール詳細: `docs/tools/nnue-training-data-guide.md`

## 用語メモ
- gap: pass1 時点の best と 2 位（best2）評価差（cp）。
- gap2: 再注釈（MultiPV=K）時点の 1 位と 2 位の評価差（cp）。
- EXACT / LOWER / UPPER: 探索の詰め判定や境界判定由来のフラグ（詳細は各ツールの README を参照）。

## CI連携の例（参考）
```bash
# 生成済み final.jsonl に対して Gate 実行（例）
cargo run --release -p tools --bin analyze_teaching_quality -- \
  runs/final.jsonl --summary --gate crates/tools/ci_gate.sample.json --gate-mode fail \
  --manifest-autoload-mode strict
```
