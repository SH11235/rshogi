# psv2jsonl: YaneuraOu PSV(yo_v1) → JSONL 直変換ツール

本ツールは YaneuraOu PackedSfenValue（PSV, yo_v1: 40B固定, 単一PV）を、本リポジトリの学習用 JSONL（最小スキーマ）へストリーミング変換します。巨大ファイルでも O(1) メモリで処理します。

- 入力: PSV（plain/gz/zst）
- 出力: JSONL（1行=1局面）
- スキーマ（yo_v1 最小）:
  - 必須: `sfen`, `eval`（`eval`は PSV の `score` をそのまま。正規化しない）
  - 任意: `mate_boundary`（`|eval| >= 31_000` のとき true）
  - 任意（`--with-pv`）: `lines: [{"score_cp": <eval>, "multipv":1, "pv":["<first_move>"]}]`（初手のみ）
  - `depth/seldepth/bound*/best2_gap_cp` は yo_v1 では出力しません

## インストール/実行

ワークスペースからビルド/実行します。

```
# 変換（STDOUTへ）
cargo run -p tools --bin psv2jsonl -- -i docs/tools/psv2jsonl/fixtures/tiny.psv -o -

# ファイルへ出力
cargo run -p tools --bin psv2jsonl -- -i docs/tools/psv2jsonl/fixtures/tiny.psv -o out.jsonl
```

圧縮入力（自動判定）:
- ファイル: マジック/拡張子で gz/zst を自動判定
- STDIN: `--decompress gz|zst` を明示

## 主なオプション

```
psv2jsonl \
  -i <IN|-> -o <OUT|-> \
  [--with-pv] [--pv-max-moves 1] \
  [--format yo_v1|auto] \
  [--strict fail-closed|allow-skip|max-errors=N] [--coerce-ply-min-1] \
  [--jobs 1] [--preserve-order] \
  [--io-buf-mb 4] [--limit N] [--sample-rate 1.0] \
  [--metrics plain|json] [--metrics-interval 5] \
  [--decompress gz|zst]
```

- `--format`: 既定 `yo_v1`。`auto` は非推奨（誤判定避け）
- `--with-pv`: `lines[0]` に初手USIを出力（yo_v1 は初手のみ）
- `--strict`:
  - `fail-closed`（既定）: 異常行で即時終了（exit=2）
  - `allow-skip`: 異常行をスキップして継続
  - `max-errors=N`: エラー合計 N 到達で終了（exit=3）
- `--coerce-ply-min-1`: `gamePly==0` を 1 に丸める（既定無効）。丸めた行はエラーとしてカウント
- `--metrics`: `plain`（進捗行更新）/`json`（1行JSON）

## エラーポリシーとログ

- 既定 `fail-closed`: 異常検出で exit=2
- スキップ時（`allow-skip`/`max-errors`）は stderr に 1行 JSON を出力（例）

```json
{"level":"error","kind":"invalid_gameply","record_index":1234,"byte_offset":49360,"detail":"gamePly=0","sfen":"..."}
```

- メトリクス（stderr）: `processed, success, skipped, errors, invalid_gameply` を定期出力

## 復号仕様（yo_v1, 32B=256bit）

- LSB-first（バイト内 bit0→bit7）。
- 手番1bit→王7bit×2→盤トークン（王以外81升）→手駒/駒箱（256bitに達するまで）。
- ハフマン表は `packed_sfen_v1.md` を参照。駒箱の金は cshogi に倣い特別扱い。
- Square 番号→USI は `f=idx/9 -> '1'+f`, `r=idx%9 -> 'a'+r`。内部 Square は `parse_usi_square` で写像。
- 盤面/手駒/手番を復元し、本エンジンの `position_to_sfen()` で整形。
- `gamePly` は PSV の値を採用（`pos.ply = (gamePly-1)*2 + (stm=='w'?1:0)`）。
  - `gamePly==0` は不正。
    - `--coerce-ply-min-1` 無効: 直ちに致命（exit=2）
    - 有効: `gamePly=1` に丸め、`coerced_gameply` の警告ログを1行出力（エラー件数に加算）

## 既知の制約

- yo_v1（40B, 単一PV）のみ対応。depth/bound/MultiPV 等は入力に無いので出力しません。
- `--with-pv` は初手のみ（Move16）。完全PVの復元は不可。

## ゴールデン検証

- 付属サンプル:
  - `tiny.psv`（800B, 20レコード）→ `expected.jsonl`（20行）
  - `tiny_bad.psv`（41B, 端数）→ `fail-closed` で exit=2

差分確認（行単位）:

```
# 変換
cargo run -p tools --bin psv2jsonl -- -i docs/tools/psv2jsonl/fixtures/tiny.psv -o out.jsonl

# 差分（行ごと）
diff -u docs/tools/psv2jsonl/fixtures/expected.jsonl out.jsonl
```

差分が空であれば成功です。

### 端数検出（fail-closed の確認）

```
# 端数（41Bなど）で致命エラー（exit=2）
target/debug/psv2jsonl -i docs/tools/psv2jsonl/fixtures/tiny_bad.psv -o /dev/null
```

## 今後の拡張プラン（スコープ外の設計メモ）

- yo_v2（将来バリアント）
  - 入力: MultiPV や depth/seldepth/nodes/time、bound（Exact/Lower/Upper）を含む拡張PSV
  - 出力(JSONL):
    - `lines`: 先頭K本（Kは `--pv-top-k`）を格納。`score_cp`, `bound`, `multipv`, `pv`, `depth`, `seldepth`
    - `best2_gap_cp`: 非mate同士のときのみ出力。mate混在時は `best2_gap_has_mate: true`
    - `bound1/2`: `lines` と常に整合（将来的には非推奨）
  - 検出: `--format yo_v2` 明示（`auto` は非推奨）。ヘッダ/サイズ/自己整合性で厳密判定。
  - エラー方針: 既定 fail-closed。`--strict` に統合。

- 出力分割/圧縮（オプション）
  - 目的: 巨大データのI/O効率・再実行耐性を向上
  - 仕様案:
    - `--split <N>`: N行ごとに出力ファイルをローテーション（`<out>.part-0001.jsonl`）
    - `--compress <gz|zst>`: 各 part を圧縮
    - `.progress` スナップショット（partごと）を併置し、部分的な再実行/検証を容易に
  - 実装: tools の既存スタイル（generate_nnue_training_data の PartWriter 相当）を流用

- 並列・パイプライン処理
  - 目的: I/O展開→復号→JSON整形→書出しの段組みでスループット向上
  - 仕様案:
    - `--jobs N`（N>1）: パイプライン並列を有効化（順序は `--preserve-order` で制御）
    - 統計/メトリクスをシャーディングし、最終で集約
  - 初版は `--jobs=1` 既定のまま（正しさ優先）

- 構造化メタ/manifest
  - 目的: 変換コマンドや入出力のハッシュ等を記録し、再現性とトレーサビリティを担保
  - 仕様案:
    - `--structured-log <PATH|->` で JSONL メタを出力
    - 入力ファイルの SHA-256/バイト数、処理件数、スキップ/エラー件数、出力の SHA-256/パート一覧
