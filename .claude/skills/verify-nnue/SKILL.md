---
name: verify-nnue
description: "NNUE モデルの正当性検証。refresh vs update 一致テスト + tatara eval_sfen とのクロス実装検証 (旧 bullet-shogi 経路は legacy) を実行する。「モデルを検証して」「Golden Forward テストして」等のリクエストに使用する。"
user-invocable: true
---

# NNUE モデル検証スキル

学習済み LayerStacks net の正当性を 2 段階で検証する。

1. rshogi 内部で差分更新 (update) と全計算 (refresh) が一致するか
2. 学習器側の実装 (tatara `eval_sfen`) と rshogi が同じ局面で同じ値を出すか

## 入力パラメータ

`$ARGUMENTS` から以下を受け取る。不足時は質問して補完する。

### 必須
- **net パス**: 検証対象の量子化済み net (`.bin`)
- **progress 係数**: progresskpabs の net なら学習時に使った係数ファイル
  (`$SHOGI_DATA/progress/<coeff>.bin`)。kingrank9 の net では不要
- **routing bucket 数**: progresskpabs の net を学習したときの bucket 数

### オプション
- **局面**: 既定は `crates/tools/docs/fixtures/search_only_sentinel_4pos.txt` の 4 局面
  (`name | sfen` 形式。SFEN 部分だけ抜き出して使う)。局面を増やしたい場合は
  `extract_bench_positions` 等で SFEN を 1 行 1 局面で用意する
- **moves**: Step 1 の手数。既定 `100`
- **threat-profile**: Threat 次元削減プロファイル。rshogi と参照実装のビルドに同一 feature を指定する必要がある
  - `threat-profile-same-class` (profile 1): 同種ペア全除外 (192,640 dims)
  - `threat-profile-same-class-major-pawn` (profile 2): 同種 + 大駒→歩除外 (173,568 dims)
  - `threat-profile-step-attacker` (profile 3): occupancy 依存 slider を attacker から除外 (33,408 dims)
  - `threat-profile-full-symdedup` (profile 4): pair 除外なし、canonical-dead な対称 edge を emit で drop (216,720 dims — full と同一。dims 不変のため net 一致は profile id で照合)
  - `threat-profile-cross-side` (profile 10): 同 side / 同種除外、敵味方跨ぎ異種のみ (96,320 dims)

## 検証手順

### Step 1: refresh vs update 一致テスト

```bash
cargo run --release -p tools --bin verify_nnue_accumulator -- \
  --nnue-file <net.bin> \
  --ls-bucket-mode progresskpabs \
  --ls-progress-buckets <routing bucket 数> \
  --ls-progress-coeff <progress.bin> \
  --moves 100
```

kingrank9 の net は `--ls-bucket-mode kingrank9` で `--ls-progress-*` を外す。

**判定**: `ALL PASSED` なら OK。`MISMATCH` があれば差分更新にバグ。

### Step 2: クロス実装検証 (tatara `eval_sfen`)

tatara checkout は環境変数 `$TATARA_DIR` があればそれを使い、無ければユーザーに場所を聞く。

```bash
# SFEN だけを 1 行 1 局面で用意する
sed -n 's/^[^#|][^|]*| //p' crates/tools/docs/fixtures/search_only_sentinel_4pos.txt > sfens.txt

# 参照値 (tatara): stderr に bucket / raw / cp を出す
cargo build --release --manifest-path "$TATARA_DIR/Cargo.toml" -p eval-sfen
args=(); while IFS= read -r s; do args+=(--sfen "$s"); done < sfens.txt
"$TATARA_DIR/target/release/eval_sfen" --nnue-file <net.bin> \
  --bucket-mode progresskpabs --progress-coeff <progress.bin> \
  --fv-scale <net の FV_SCALE> --debug "${args[@]}"

# rshogi: TSV (sfen / bucket / raw / score / score_cp) を stdout に出す
cargo run --release -p tools --bin eval_sfens -- \
  --nnue <net.bin> --sfens sfens.txt \
  --bucket-mode progresskpabs --progress-coeff <progress.bin> \
  --progress-buckets <routing bucket 数> --count <局面数>
```

**判定**: 全局面で `bucket` と `raw` が完全一致すれば OK。`raw` は FV_SCALE で割る前の
ネットワーク出力なので、FV_SCALE の指定違いに左右されずに比べられる
(tatara の `cp` は rshogi の `score` 列に対応する)。

- **bucket が食い違う**: tatara は net に格納された bucket 数で routing する。
  rshogi の `--progress-buckets` が格納数と違う (格納 9 / routing 8 の net 等) と bucket が
  ずれる。routing 数を確認し、格納数と routing 数が異なる net はこの経路で比べない
  (Step 1 と legacy 経路で検証する)
- **tatara が読めない arch**: `eval_sfen` が読み込みエラーを出す arch は legacy 経路で検証する
- 学習が数 superbatch しか進んでいない net は重みが異常に大きく、どこかの accumulator で
  飽和して偽の不一致に見えることがある。十分学習した net で検証する

---

## Legacy: bullet-shogi とのクロス検証

bullet-shogi で学習した旧 net の再検証にだけ使う。新しい net は Step 2 の tatara 経路を使う。

### 参照値生成 (bullet)

```bash
cd /path/to/bullet-shogi
cargo run --release --example shogi_layerstack_eval -- \
  --checkpoint <checkpoint_dir> \
  --pack <教師 PSV (例: $SHOGI_DATA/teachers/DLSuisho15b_aoba_deduped_shuffled.bin)> \
  --integer-forward --samples 1 \
  --bucket-mode progress8kpabs \
  --progress-coeff <progress.bin> \
  [--threat]
```

- `checkpoint_dir` を省略するときは net の親ディレクトリ
- bullet 側 CLI は旧名 `progress8kpabs` のままなので rshogi 側の名前に置換しない
- `--threat` は Threat モデル時に必須 (`get_active_features` の入力型分岐に必要)。
  PSQT の有無は arch_str から自動検出される
- Threat profile を使う net は `--no-default-features --features cpu,<profile feature>` で
  bullet を build する

出力から `SFEN:` と `final_score:` を記録する。

### rshogi 側で同一局面を評価

```bash
cargo build --release -p rshogi-usi --features diagnostics

printf 'setoption name EvalFile value <net.bin>
setoption name LS_BUCKET_MODE value progresskpabs
setoption name LS_PROGRESS_BUCKETS value <routing bucket 数>
setoption name LS_PROGRESS_COEFF value <progress.bin>
isready
position sfen <bullet 出力の SFEN>
eval diag
quit
' | RUST_LOG=info ./target/release/rshogi-usi 2>&1 | grep "score:"
```

Threat profile 付き net では rshogi も同じ profile feature で build する:

```bash
cargo build --release -p tools --no-default-features \
  --features layerstacks-768x16x32,nnue-threat,threat-profile-same-class
cargo build --release -p rshogi-usi --no-default-features \
  --features rshogi-core/layerstack-arch,rshogi-core/layerstacks-768x16x32,rshogi-core/nnue-threat,rshogi-core/threat-profile-same-class,rshogi-usi/layerstack-arch,rshogi-usi/nnue-threat,rshogi-usi/threat-profile-same-class,rshogi-usi/diagnostics
```

`threat-profile-same-class` は対象 profile の feature に全て置き換える。

**判定**: `final_score` が完全一致すれば OK。

## 不一致時のデバッグ

rshogi の中間値は `eval_sfens --dump-debug-first` (最初の局面) か、diagnostics build の
`eval diag` で出す。legacy 経路で bullet と比べる場合の対応:

| 中間値 | bullet 出力キー | rshogi ログキー |
|--------|-----------------|-----------------|
| FT accumulator | `FT acc[stm] first 8` | `us_combined (piece+threat) first 16` ※Threat 時 |
| | | `us_acc (piece) first 16` ※非Threat 時 |
| Product Pooling | `PP out first 8` | `transformed first 32` |
| L1 出力 | `L1 out` | `l1_out` |
| L1 skip | `L1 skip` | `l1_skip` |
| raw_score | `raw_score` | `raw_score (with skip)` |
| PSQT value | `psqt_value` | `psqt_value` |
| 最終スコア | `final_score` | `score:` の最初の数値 |

### よくある原因

- **bucket だけ不一致**: routing bucket 数か progress 係数ファイルの取り違え
- **FT accumulator が不一致**: feature index 計算の差異、weight layout の転置
- **PSQT だけ不一致**: PSQT の read サイズが input_size vs halfka_dim で不整合
- **final_score だけ不一致**: fv_scale の差異、PSQT /2 の有無
- **rshogi diagnostics が Threat を反映しない**: `evaluate_with_diagnostics` の Threat 結合漏れ

## 注意事項

- rshogi の `eval diag` は `--features diagnostics` の build が必要
- `NNUE_ARCHITECTURE` USI オプションは**指定しない**（arch_str から自動検出させる）
- **Threat profile は rshogi と参照実装の両方のビルドで必ず同一 feature を指定すること**。
  不一致だと THREAT_DIMENSIONS が異なり weight 読み込みが破壊的にずれる。
  profile 0 (全ペア) はデフォルトで feature 指定不要
