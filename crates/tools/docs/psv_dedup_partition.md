# psv_dedup_partition

`psv_dedup_partition` は、大規模な PSV データから PackedSfen 重複を **完全一致 (exact)** で除去する低メモリ向けツールです。

- 入力: PackedSfenValue 40 バイト固定長ファイル
- 重複キー: 先頭 32 バイトの PackedSfen（`psv_dedup` と同じ方針）
- 出力: 最初に出現した局面のみを残した単一ファイル
- 方式: ディスクパーティショニング → パーティションごとに `HashSet<[u8; 32]>`
- 特徴: **偽陽性・偽陰性なし**、メモリは「最大パーティション分」に抑えられる

巨大データでも exact に dedup したいが、`psv_dedup`（全件 HashSet 保持）ではメモリが足りないケース向けです。

## 仕組み

### Phase 1: Partitioning

入力を順次ストリーミングで読み、PackedSfen (32 B) の 64 bit FNV-1a ハッシュを計算して `--partitions` 個の一時ファイルに振り分ける。

```
入力 .bin → hash_packed_sfen(sfen) % P → partition_NNNNN.bin
```

同じ局面は必ず同じパーティションに入るため、後段で「1 パーティション内で HashSet 判定」すれば全体の dedup と等価になる。

### Phase 2: Deduplication

各パーティションファイルを 1 つずつ読み込み、`HashSet<[u8; 32]>` で first-wins の重複判定を行い、出力ファイルに追記する。処理済みパーティションは `HashSet` ごと解放してから次へ進むため、ピークメモリは「最大パーティション 1 つぶんのユニーク局面」で決まる。

`--keep-temp` を指定しない限り、処理済みパーティションは逐次削除される。

### 一時ディレクトリ構造

```
<temp-dir>/
├── input/
│   ├── partition_00000.bin
│   ├── partition_00001.bin
│   └── ...
└── ref/              # --reference 指定時のみ
    ├── partition_00000.bin
    └── ...
```

## reference モード

既存の dedup 済みファイルがあり、新規データから「その既存ファイルに含まれない局面」だけを抽出したい場合に `--reference` を使う。参照ファイルは出力に含まれず、HashSet 登録のみ行われる。

Phase 2 の流れ (パーティションごと):

1. `ref/partition_{i}.bin` を HashSet に全件ロード（出力しない）
2. `input/partition_{i}.bin` を streaming し、HashSet に未登録なら出力 + 挿入
3. HashSet を解放して次パーティションへ

これにより、既存ファイルと新規ファイルを結合してから dedup するよりも I/O が少なく、かつ reference 側の重複は出力に回らない。`psv_dedup_bloom --reference` の完全一致版に相当。

`--phase2-only` で再開する際は `<temp-dir>/ref/` の存在を自動検出し、reference モードとして処理する（`--reference` 再指定は不要）。

## 事前見積りと不足チェック

起動時に Phase 1 / Phase 2 のメモリとディスク要件を見積もり、不足時は実行前に停止する。

```
=== Resource Estimate ===
Total input records:  17000000000 (680000000000 bytes / 40)
Phase 1 temp disk:    633.2 GiB (cleaned up on success)
Phase 1 memory:       0.5 GiB (fixed: partitions × buffer)
Phase 2 peak memory:  1.3 GiB (HashSet of largest partition, ~1.20x variance)
Memory available:     64.0 GiB (threshold 80% = 51.2 GiB)
Temp disk available:  8.0 TiB (/fast/ssd/tmp)
Output disk:          same filesystem as temp (/fast/ssd). 追加 headroom ...
```

チェック内容:

- **メモリ**: `Phase1 mem` と `Phase2 peak mem` のうち大きい方が `MemAvailable × 80%` を超えたら Err
- **temp ディスク**: 入力合計 × 1.05 が `--temp-dir` の空きを超えたら Err
- **output ディスク**: 出力上限 = 入力合計（dedup しない最悪ケース）× 1.05 が出力親ディレクトリの空きを超えたら Err。temp と同一ファイルシステムの場合もチェックは省略しない。`--keep-temp` なしでは「処理中の最大 input partition」ぶんの追加 headroom、`--keep-temp` ありでは output 全量ぶんの追加空き容量を要求する
- 不足時は停止。`--force` を付けると Warning を出して続行する（swap 多用・途中失敗のリスクを許容する場合のみ）

## 一時ディスクの寿命

- **Phase 1 実行中〜Phase 2 開始時点**: peak 使用量 ≈ 入力合計サイズ
- **Phase 2 実行中**: partition ごとに処理→削除するので、temp 使用量は徐々に減る
- **完走時**: 一時ファイルと一時ディレクトリを全て削除（`--keep-temp` 時のみ保持）
- **出力ファイル**: 別途 `--output` に書き出され、完走後も残る

## メモリ見積り

Phase 2 のピークメモリはおおよそ次の式で決まる:

```
peak_memory ≈ (total_records / partitions) × entry_overhead
```

`entry_overhead` は `HashSet<[u8; 32]>` で 1 エントリあたり 50〜70 B 程度（バケット + エントリ + load factor）。

### 参考値（均等分布を仮定）

| 総レコード数 | `--partitions 1024` | `--partitions 4096` |
|---:|---:|---:|
| 10 億      | ~60 MiB       | ~15 MiB       |
| 100 億     | ~600 MiB      | ~150 MiB      |
| 250 億     | ~1.5 GiB      | ~370 MiB      |

Phase 1 の固定コストは `partitions × partition_buffer_kb`。デフォルト (`1024 × 64 KiB = 64 MiB`) で十分で、メモリが更に厳しい場合は `--partition-buffer-kb 16` 等に縮小できる。

## 一時ディスク

Phase 1 は入力と **ほぼ同サイズ** のデータを一時ディレクトリに書き出す。`--temp-dir` は入力と同等以上の空き容量がある場所を指定すること（デフォルト: `./psv_dedup_partition_tmp`）。

Phase 2 の進行に合わせて一時ファイルは削除されるので、ピーク使用量は入力サイズ程度。`--keep-temp` 指定時は残る。

## I/O

合計 I/O は通常 dedup の約 2 倍（Phase 1 で write once、Phase 2 で read once）。一方 `psv_dedup_bloom` は 1 パスなので、「メモリ vs ディスク I/O」のトレードオフになる。

## FD 上限

`--partitions 1024` 指定時は ulimit の soft limit が 1024 以上必要。起動時にチェックして不足なら警告する。Linux のデフォルトは 1024 なので、多くの環境で次の指定が必要:

```bash
ulimit -n 4096
```

## 使用例

### 基本: ディレクトリ内の全ファイルを exact dedup

```bash
cargo run --release -p tools --bin psv_dedup_partition -- \
  --input-dir ../bullet-shogi/data/DLSuisho15b \
  --pattern "*.bin" \
  --output /path/to/deduped.bin \
  --temp-dir /fast/ssd/psv_tmp
```

### メモリをさらに絞る

```bash
cargo run --release -p tools --bin psv_dedup_partition -- \
  --input-dir /path/to/dir \
  --output deduped.bin \
  --temp-dir /fast/ssd/psv_tmp \
  --partitions 4096 \
  --partition-buffer-kb 16
```

### 既存 dedup 済みファイルとの差分だけ抽出 (reference モード)

```bash
cargo run --release -p tools --bin psv_dedup_partition -- \
  --reference existing_deduped.bin \
  --input new_data.bin \
  --output unique_new.bin \
  --temp-dir /fast/ssd/psv_tmp
```

複数の reference を指定する場合はカンマ区切り:

```bash
  --reference old1.bin,old2.bin,old3.bin \
```

### Phase 1 だけ先に済ませて後日 Phase 2

`--keep-temp` で一時ファイルを保持し、別セッションで `--phase2-only` から再開できる。途中で partition ファイルが欠けた temp ディレクトリは破損扱いとなり、`--phase2-only` は即エラーで停止する。

```bash
# セッション1: 振り分けだけ
cargo run --release -p tools --bin psv_dedup_partition -- \
  --input-dir /path/to/dir \
  --output deduped.bin \
  --temp-dir ./psv_tmp \
  --keep-temp

# セッション2: 後日 Phase 2 のみ
cargo run --release -p tools --bin psv_dedup_partition -- \
  --output deduped.bin \
  --temp-dir ./psv_tmp \
  --phase2-only
```

## オプション一覧

| オプション | 説明 | デフォルト |
|---|---|---|
| `--reference` | 参照ファイル（カンマ区切り）。HashSet に登録するが出力しない | — |
| `--input` | 入力ファイル（カンマ区切り）。`--input-dir` と排他 | — |
| `--input-dir` | 入力ディレクトリ。`--pattern` と組み合わせ | — |
| `--pattern` | `--input-dir` 使用時の glob パターン | `*.bin` |
| `--output` | 出力ファイルパス | — |
| `--temp-dir` | パーティション一時ファイルの置き場 | `./psv_dedup_partition_tmp` |
| `--partitions` | パーティション数 | `1024` |
| `--partition-buffer-kb` | 各パーティションの BufWriter バッファ (KiB) | `64` |
| `--max-positions` | 処理する入力レコードの最大件数（0 = 全件、試走用）。参照は常に全件 | `0` |
| `--phase2-only` | Phase 1 をスキップして既存一時ファイルから再開（ref/ は自動検出） | off |
| `--keep-temp` | 完了後も一時ファイル・ディレクトリを削除しない | off |
| `--force` | メモリ/ディスク不足でも警告のみで続行する | off |

## `psv_dedup` / `psv_dedup_bloom` との比較

| 項目 | `psv_dedup` | `psv_dedup_bloom` | `psv_dedup_partition` |
|---|---|---|---|
| 方式 | 全件 `HashSet<u64>` | Blocked Bloom Filter | ディスクパーティション + `HashSet<[u8;32]>` |
| 正確性 | ほぼ exact (64bit hash 衝突のみ) | 近似 (`--fpr` で制御、偽陽性あり) | **完全 exact** |
| メモリ | ユニーク局面数 × 約 16 B | 固定 (入力規模と `fpr` から決定) | 最大パーティションのユニーク局面ぶん |
| 一時ディスク | 不要 | 不要 | 入力と同等 |
| I/O パス数 | 1 パス | 1 パス | 2 パス (write + read) |
| 向いている規模 | 数億〜数十億 | 数百億 (メモリ潤沢) | 数十億〜数百億 (メモリ限定) |

選び方は [pack_tools.md の「重複除去ツールの選び方」](pack_tools.md#重複除去ツールの選び方) を参照。

## 注意

- 入力と出力が同一ファイルの場合はエラー
- キーは PackedSfen のみ。同一局面に対する複数の教師手がある場合は `psv_dedup` と同様、最初の出現だけ残す
- パーティション出力の順序は保存されない（ハッシュ順）。順序を保ちたい場合は後段で `shuffle_psv` 等を使う
