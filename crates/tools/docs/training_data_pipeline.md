# 教師データパイプライン

既存の教師データに新規データを追加する際の手順ガイドです。

個別ツールの詳細は以下を参照してください:
- [psv_dedup_bloom](psv_dedup_bloom.md) — ブルームフィルタ重複除去
- [shuffle_psv](shuffle_psv.md) — シャッフル

## 前提

- 教師データは PackedSfenValue 形式（40 バイト/レコード）
- 既存データは重複除去・シャッフル済みとする

## パイプライン概要

```
新規データ
    │
    ▼
[1. 差分 dedup] ── 既存データを参照して重複除去
    │
    ▼
unique_new.bin (既存に含まれない新規レコードのみ)
    │
    ▼
[2. 結合] ── 既存データ + unique_new.bin
    │
    ▼
combined.bin
    │
    ▼
[3. シャッフル]
    │
    ▼
最終教師データ.bin
```

## 手順

### 1. 差分 dedup

既存データから bloom filter をメモリ上に構築し、新規データの重複を除去します。
`--reference` モードを使うことで、既存データの再書き出しが不要になり I/O を削減できます。

```bash
psv_dedup_bloom \
  --reference existing_deduped_shuffled.bin \
  --input new_data.bin \
  --output unique_new.bin
```

**リソース目安** (既存 146 億 + 新規 19 億レコードの場合):
- bloom filter メモリ: 約 28 GiB (FPR=0.001)
- 処理時間: 約 100 分（既存読み込み 85 分 + 新規フィルタ 15 分）
- ディスク: 出力ファイル分のみ（入力サイズ以下）

### 2. 結合

```bash
cat existing_deduped_shuffled.bin unique_new.bin > combined.bin
```

**注意**: `cat A B >> A` のように入力と出力を同じファイルにしないでください。

### 3. シャッフル

```bash
TMPDIR=/path/to/fast/disk shuffle_psv \
  --input combined.bin \
  --output final_deduped_shuffled.bin \
  --seed 42 \
  --chunk-size 1000000000
```

**リソース目安** (165 億レコード = 617 GiB の場合):
- メモリ: チャンクサイズ × 40 bytes（chunk_size=10 億 → 約 37 GiB）
- 処理時間: 約 60 分（Pass 1: 30 分 + Pass 2: 30 分）
- ディスク: 入力 + 一時ファイル(≈入力) + 出力 = **入力の約 3 倍**

### 4. 中間ファイル削除

```bash
rm unique_new.bin combined.bin
# 元データが不要なら:
rm new_data.bin
```

## チャンクサイズの選び方

`--chunk-size` はシャッフル時のチャンクあたりレコード数です。メモリに載る最大値を選んでください。

| メモリ | 推奨 chunk_size | チャンクメモリ |
|---|---|---|
| 32 GiB | 500,000,000 | 約 18.6 GiB |
| 64 GiB | 1,000,000,000 | 約 37.3 GiB |
| 128 GiB | 2,000,000,000 | 約 74.5 GiB |
| 256 GiB | 4,000,000,000 | 約 149.0 GiB |

Pass 2 では複数チャンクを並列処理しますが、バッチサイズは利用可能メモリの 70% に基づいて自動制限されます。メモリ不足の場合は処理開始前にエラーで停止します（`--force` で強制続行可能）。

## TMPDIR について

チャンク方式のシャッフルでは一時ファイルが `TMPDIR` に作成されます。

- デフォルトは `/tmp`（root FS）
- 大規模データでは root FS の容量が不足する場合がある
- **入力ファイルと同じディスク上のディレクトリを `TMPDIR` に指定**することを推奨

```bash
mkdir -p /mnt/data/tmp
TMPDIR=/mnt/data/tmp shuffle_psv ...
```

## 実行例

547 GiB の既存教師データに 70 GiB の新規データを追加した実例:

```bash
# 1. 差分 dedup (96 分)
#    既存 146.6 億レコード → bloom 27.7 GiB
#    新規 18.8 億レコード → 0.92% が重複、99.08% が新規
psv_dedup_bloom \
  --reference /data/DLSuisho15b_deduped_shuffled.bin \
  --input /data/aoba_rescore_dlsuisho.bin \
  --output /data/aoba_rescore_unique.bin

# 2. 結合
cat /data/DLSuisho15b_deduped_shuffled.bin \
    /data/aoba_rescore_unique.bin \
    > /data/DLSuisho15b_aoba_combined.bin

# 3. シャッフル (60 分, 17 チャンク)
TMPDIR=/data/tmp shuffle_psv \
  --input /data/DLSuisho15b_aoba_combined.bin \
  --output /data/DLSuisho15b_aoba_deduped_shuffled.bin \
  --seed 42 --chunk-size 1000000000

# 4. 中間ファイル削除
rm /data/aoba_rescore_unique.bin /data/DLSuisho15b_aoba_combined.bin
```
