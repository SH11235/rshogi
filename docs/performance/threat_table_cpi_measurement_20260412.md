# Threat テーブル cache pressure 定量計測 (2026-04-12)

`search_only_ab` を用いて v87/v91/v92/v93/v94 の cycles/node・instructions/node・CPI を計測し、
Threat テーブル削減（profile 変更）の NPS 改善寄与を **計算量削減** と **cache pressure 削減** に分解した記録。

この計測は Threat テーブル改修の方針決定の根拠となる。

## 背景

- `docs/performance/threat_table_size_reference.md` で Threat テーブルサイズと L3 キャッシュの関係を議論
- 疑問: 「L3=64 MB に収めること」は本当に有効か？ (History 群 ~92 MB だけで L3 超過、FT weights も 72 MB+)
- 仮説: cache pressure 削減の効果は NPS ベースで限定的ではないか

`(cat <<EOF ... sleep N; echo quit) | perf stat -- engine` 方式では初期化コスト混入 + タイミング依存で再現性がなく失敗。
`crates/tools/src/bin/search_only_ab.rs` を使うことで **探索区間のみ** の HW カウンタを厳密に取得できた。

## 計測手法

ツール: `crates/tools/src/bin/search_only_ab.rs`

- `perf stat --control fd:20,21 -D -1` (計測 disabled で start)
- USI: `usi` → option 列挙受信 → `setoption` → `isready` → `readyok`
- **計測区間 = `enable` → `position ... go movetime ...` → `bestmove` → `disable`**
- ACK 同期で初期化コストを完全排除
- baseline vs candidate を `abba` 順序で 2 round、計 32 run/ペア

詳細手順は `.claude/skills/usi-perf-measure/SKILL.md` を参照。

## 環境

- **CPU**: AMD Ryzen 9 5950X (16C/32T, Zen 3)
- **L3**: 64 MB 総計 (CCX ごとに 32 MB、8 コア共有)
- **turbo boost**: 有効
- **governor**: schedutil
- **kernel**: Linux 6.8.0-90-generic
- **commit**: `c5e4e057` (全バイナリ統一)
- **build profile**: production (`lto=fat, cgu=1, overflow-checks=false`)

### 計測時の並行プロセス対処

計測中に `shogi_layerstack` (v94 学習, 14 スレッド, 733% CPU) が稼働していた。
L3 汚染を避けるため **CCX 分離** を適用:

```bash
# 学習プロセスを CCX1 (コア 8-15) に追いやる
taskset -ap -c 8-15 <shogi_layerstack_pid>

# 計測は CCX0 のコア 0 で pin
--cpu 0  (search_only_ab オプション)
```

`engine_selfplay` (PID 1542244) は CPU 0% で idle のため無視。

## バイナリとモデル

全て commit `c5e4e057` の production build。

| ラベル | バイナリ | モデル | L0 | Threat profile | Threat dims | テーブルサイズ |
|---|---|---|---:|---|---:|---:|
| v87 | `/tmp/rshogi-1536-progdiff-c5e4e057-prod` | `checkpoints/v87/v87-60/quantised.bin` | 1536 | なし | 0 | **0 MB** |
| v91 | `/tmp/rshogi-768-threat-p0-c5e4e057-prod` | `checkpoints/v91/v91-60/quantised.bin` | 768 | profile 0 (full) | 216,720 | **159 MB** |
| v92 | `/tmp/rshogi-512-threat-p0-c5e4e057-prod` | `checkpoints/v92/v92-60/quantised.bin` | 512 | profile 0 (full) | 216,720 | **106 MB** |
| v93 | `/tmp/rshogi-768-threat-p1-c5e4e057-prod` | `checkpoints/v93/v93-60/quantised.bin` | 768 | profile 1 (same-class) | 192,640 | **141 MB** |
| v94 | `/tmp/rshogi-512-threat-p10-c5e4e057-prod` | `checkpoints/v94/v94-60/quantised.bin` | 512 | cross-side | 96,320 | **47 MB** |

モデルパスの root: `/mnt/nvme1/development/bullet-shogi/`

## 局面ファイル

`/tmp/search_only_sentinel_4pos.txt`:

```
hirate-like | lnsgkgsnl/1r7/p1ppp1bpp/1p3pp2/7P1/2P6/PP1PPPP1P/1B3S1R1/LNSGKG1NL b - 9
complex-middle | l4S2l/4g1gs1/5p1p1/pr2N1pkp/4Gn3/PP3PPPP/2GPP4/1K7/L3r+s2L w BS2N5Pb 1
tactical | 6n1l/2+S1k4/2lp4p/1np1B2b1/3PP4/1N1S3rP/1P2+pPP+p1/1p1G5/3KG2r1 b GSN2L4Pgs2p 1
movegen-heavy | l6nl/5+P1gk/2np1S3/p1p4Pp/3P2Sp1/1PPb2P1P/P5GS1/R8/LN4bKL w RGgsn5p 1
```

## 再現コマンド

### 1 ペア計測 (v92 vs v94 の例)

```bash
cd /mnt/nvme1/development/rshogi

./target/release/search_only_ab \
  --baseline  /tmp/rshogi-512-threat-p0-c5e4e057-prod \
  --candidate /tmp/rshogi-512-threat-p10-c5e4e057-prod \
  --positions /tmp/search_only_sentinel_4pos.txt \
  --movetime-ms 10000 \
  --pattern abba \
  --rounds 2 \
  --threads 1 \
  --hash-mb 256 \
  --cpu 0 \
  --material-level none \
  --baseline-usi-option  EvalFile=/mnt/nvme1/development/bullet-shogi/checkpoints/v92/v92-60/quantised.bin \
  --candidate-usi-option EvalFile=/mnt/nvme1/development/bullet-shogi/checkpoints/v94/v94-60/quantised.bin \
  --usi-option LS_BUCKET_MODE=progress8kpabs \
  --usi-option LS_PROGRESS_COEFF=/mnt/nvme1/development/bullet-shogi/data/progress/nodchip_progress_e1_f1_cuda.bin \
  --usi-option FV_SCALE=28 \
  --json-out /tmp/perf_v92_vs_v94.json
```

### 5 ペア一括実行スクリプト

本計測で使用した `/tmp/run_perf_measure.sh` (v92 vs v94 と v87 baseline × 4 candidates):

```bash
#!/bin/bash
set -euo pipefail

SEARCH=/mnt/nvme1/development/rshogi/target/release/search_only_ab
POSITIONS=/tmp/search_only_sentinel_4pos.txt
OUT_DIR=/tmp/perf_measure_20260412
mkdir -p "$OUT_DIR"

BIN_v87=/tmp/rshogi-1536-progdiff-c5e4e057-prod
BIN_v91=/tmp/rshogi-768-threat-p0-c5e4e057-prod
BIN_v92=/tmp/rshogi-512-threat-p0-c5e4e057-prod
BIN_v93=/tmp/rshogi-768-threat-p1-c5e4e057-prod
BIN_v94=/tmp/rshogi-512-threat-p10-c5e4e057-prod

MODEL_v87=/mnt/nvme1/development/bullet-shogi/checkpoints/v87/v87-60/quantised.bin
MODEL_v91=/mnt/nvme1/development/bullet-shogi/checkpoints/v91/v91-60/quantised.bin
MODEL_v92=/mnt/nvme1/development/bullet-shogi/checkpoints/v92/v92-60/quantised.bin
MODEL_v93=/mnt/nvme1/development/bullet-shogi/checkpoints/v93/v93-60/quantised.bin
MODEL_v94=/mnt/nvme1/development/bullet-shogi/checkpoints/v94/v94-60/quantised.bin

COMMON_OPTS=(
  --positions "$POSITIONS"
  --movetime-ms 10000
  --pattern abba
  --rounds 2
  --threads 1
  --hash-mb 256
  --cpu 0
  --material-level none
  --usi-option LS_BUCKET_MODE=progress8kpabs
  --usi-option LS_PROGRESS_COEFF=/mnt/nvme1/development/bullet-shogi/data/progress/nodchip_progress_e1_f1_cuda.bin
  --usi-option FV_SCALE=28
)

run_pair() {
  local base_name=$1 base_bin=$2 base_model=$3
  local cand_name=$4 cand_bin=$5 cand_model=$6
  local label="${base_name}_vs_${cand_name}"

  "$SEARCH" \
    --baseline  "$base_bin" \
    --candidate "$cand_bin" \
    "${COMMON_OPTS[@]}" \
    --baseline-usi-option  "EvalFile=$base_model" \
    --candidate-usi-option "EvalFile=$cand_model" \
    --json-out "$OUT_DIR/${label}.json" 2>&1 | tee "$OUT_DIR/${label}.log"
}

run_pair v92 "$BIN_v92" "$MODEL_v92"  v94 "$BIN_v94" "$MODEL_v94"
run_pair v87 "$BIN_v87" "$MODEL_v87"  v91 "$BIN_v91" "$MODEL_v91"
run_pair v87 "$BIN_v87" "$MODEL_v87"  v92 "$BIN_v92" "$MODEL_v92"
run_pair v87 "$BIN_v87" "$MODEL_v87"  v93 "$BIN_v93" "$MODEL_v93"
run_pair v87 "$BIN_v87" "$MODEL_v87"  v94 "$BIN_v94" "$MODEL_v94"
```

### 集計 (jq)

```bash
cd /tmp/perf_measure_20260412
for f in v92_vs_v94 v87_vs_v91 v87_vs_v92 v87_vs_v93 v87_vs_v94; do
  echo "=== $f ==="
  jq -r '.summary | "  baseline : nps=\(.baseline.average_nps)  cpn=\(.baseline.cycles_per_node | (. * 10 | round / 10))  ipn=\(.baseline.instructions_per_node | (. * 10 | round / 10))  depth=\(.baseline.average_depth | (. * 100 | round / 100))",
  "  candidate: nps=\(.candidate.average_nps)  cpn=\(.candidate.cycles_per_node | (. * 10 | round / 10))  ipn=\(.candidate.instructions_per_node | (. * 10 | round / 10))  depth=\(.candidate.average_depth | (. * 100 | round / 100))",
  "  delta    : nps=\(.nps_delta_pct | (. * 100 | round / 100))%  cpn=\(.cycles_per_node_delta_pct | (. * 100 | round / 100))%  ipn=\(.instructions_per_node_delta_pct | (. * 100 | round / 100))%"
  ' "$f.json"
done
```

## 計測結果

movetime 10000 ms × pattern abba × rounds 2 × 4 positions = 32 run/ペア。
JSON 出力は `/tmp/perf_measure_20260412/*.json` に保存。

### ペアごとの delta

| ペア | Δ NPS | Δ cycles/node | Δ instructions/node |
|---|---:|---:|---:|
| **v92→v94** (同一 L0=512, full→cross-side) | **+9.1%** | **-8.33%** | **-5.88%** |
| v87→v91 (L0 1536→768, +profile 0) | -31.59% | +46.19% | +38.33% |
| v87→v92 (L0 1536→512, +profile 0) | -22.37% | +28.79% | +23.87% |
| v87→v93 (L0 1536→768, +profile 1) | -29.90% | +42.61% | +38.30% |
| v87→v94 (L0 1536→512, +cross-side) | -15.06% | +17.74% | +16.67% |

### 絶対値と CPI

CPI = cycles/node ÷ instructions/node。cache miss 等で命令実行が stall すると増える指標。
命令数の違い（計算量差）を除いた **cache pressure の純粋な指標** として機能する。

| 構成 | Threat テーブル | cycles/node | instructions/node | **CPI** | vs v87 CPI |
|---|---:|---:|---:|---:|---:|
| v87 (L0=1536, Threat なし) | 0 MB | 7,424 | 15,860 | **0.468** | — |
| **v94 (L0=512, cross-side)** | **47 MB (L3 以下)** | 8,741 | 18,493 | **0.473** | **+1.1%** |
| v92 (L0=512, profile 0) | 106 MB | 9,555 | 19,651 | 0.486 | +3.8% |
| v93 (L0=768, profile 1) | 141 MB | 10,569 | 21,933 | 0.482 | +2.9% |
| v91 (L0=768, profile 0) | 159 MB | 10,804 | 21,935 | 0.493 | +5.3% |

## 分析

### 1. v92 → v94 の NPS +9.1% の内訳

- **instructions/node -5.88%**: cross-side で Threat dims が半減 (216,720 → 96,320) → 計算量削減
- **CPI -2.7%** (0.486 → 0.473): cache pressure 削減
- **寄与比: 計算量 : cache ≈ 2 : 1**

cross-side 化の NPS 改善のうち、2/3 は計算量削減由来で、cache pressure 削減由来は 1/3 程度。

### 2. テーブルサイズと CPI の関係

v87 (Threat なし) との CPI 差で cache pressure 追加分を見ると:

| 構成 | テーブルサイズ | CPI 増加 (vs v87) |
|---|---:|---:|
| v94 | 47 MB (L3 以下) | +1.1% |
| v92 | 106 MB (L3 超) | +3.8% |
| v93 | 141 MB (L3 超) | +2.9% |
| v91 | 159 MB (L3 超) | +5.3% |

テーブルサイズが小さいほど CPI 増加が小さい傾向はあるが、**単調ではない**。
v92 (106 MB) と v93 (141 MB) は逆転しており、サイズのみでは説明できない。
profile の内容 (dims 分布) や L0 との相互作用が影響している。

#### v92 vs v94 の CPI 差の解釈（注意）

v92 (106 MB, L3 超) → v94 (47 MB, L3 以下) の CPI は -2.8% (0.486 → 0.473)、
NPS 換算で **+2.8% 相当** の改善寄与。

ただしこれを**「L3 境界を跨いだ単独効果」と即断することはできない**:

- v92 と v94 は **profile と dims 分布の両方が違う** (full 216,720 dims vs cross-side 96,320 dims)
- 同一 profile のまま L3 境界を跨ぐ比較データは今回存在しない
- CPI delta には cache 以外の要因 (TLB miss、memory layout、prefetcher 動作) も含まれる

ペアの選び方で数字も変動する:

| ペア | Δ テーブルサイズ | Δ CPI | NPS 換算 |
|---|---|---:|---:|
| v91 (159 MB) → v94 (47 MB) | -112 MB | -4.2% | +4.2% |
| v92 (106 MB) → v94 (47 MB) | -59 MB | -2.8% | +2.8% |
| v93 (141 MB) → v94 (47 MB) | -94 MB | -1.9% | +1.9% |

現データから言えるのは「**100 MB 台 → 50 MB 未満の範囲のテーブルサイズ削減で、
cache 関連の寄与は NPS +2〜4% 程度のオーダー**」という上限目安のみ。
**「L3 以下にすること」の単独効果は本計測からは分離できない**。

### 3. Threat 導入の NPS コストは計算量が主因

v87 vs 各 Threat candidate の NPS 差 (-15〜-32%) のうち:

- **instructions/node 差 (+17〜+38%) が支配的**
- CPI 差 (+1〜+5%) は従

つまり **Threat accumulate の命令数コストが NPS への主要ボトルネック**。
テーブルサイズ削減より accumulate 処理の高速化の方がインパクトが大きい可能性。

### 4. v94 (cross-side) が棋力で正当化されない理由

225 局の selfplay で v94 は v92 に対し Elo +14 ±45 の負け越し（五分〜不利）。

計測結果からの説明:

- NPS -15.1% (v87 比) のうち、計算量増 +16.7% に対し cache 削減は +1.1%
- **dims 半減（情報量半減）のペナルティが eval 品質に直撃**
- cache pressure 削減（+1.1%）では補填不可能

## perf record による hotspot 分析 (追加計測)

CPI 分解で「dims を削っても instructions/node がほぼ減らない」という現象が観測された
(v92 vs v94: dims -55.5% → ipn -5.88%)。これを裏付けるため、関数レベルの hotspot を
perf record で取得した。

### 計測環境

- バイナリ: v92 相当を profiling profile で再 build (debug symbol 付き)
  ```bash
  RUSTFLAGS="-C target-cpu=native -C force-frame-pointers=yes" \
    cargo build --profile profiling -p rshogi-usi --bin rshogi-usi \
    --features layerstack-only,layerstacks-512,nnue-threat
  ```
  → `target/profiling/rshogi-usi` (L0=512, profile 0, debug symbol あり)
- 局面: `l4S2l/4g1gs1/5p1p1/pr2N1pkp/4Gn3/PP3PPPP/2GPP4/1K7/L3r+s2L w BS2N5Pb 1` (中盤)
- 探索: `go movetime 20000` (1 回、decisive な hotspot 把握目的)
- CPU pinning: `taskset -c 0`
- perf: `-g --call-graph=dwarf -F 997`
- サンプル数: 20,498 (cycles:P イベント)

### 再現コマンド

```bash
(cat <<'EOF'
usi
setoption name EvalFile value /mnt/nvme1/development/bullet-shogi/checkpoints/v92/v92-60/quantised.bin
setoption name LS_BUCKET_MODE value progress8kpabs
setoption name LS_PROGRESS_COEFF value /mnt/nvme1/development/bullet-shogi/data/progress/nodchip_progress_e1_f1_cuda.bin
setoption name MaterialLevel value none
setoption name FV_SCALE value 28
setoption name Threads value 1
setoption name USI_Hash value 256
isready
position sfen l4S2l/4g1gs1/5p1p1/pr2N1pkp/4Gn3/PP3PPPP/2GPP4/1K7/L3r+s2L w BS2N5Pb 1
go movetime 20000
EOF
sleep 25
echo quit) | taskset -c 0 perf record -g --call-graph=dwarf -F 997 \
  -o /tmp/threat_profile_v92.data \
  -- /mnt/nvme1/development/rshogi/target/profiling/rshogi-usi

# 集計
perf report -i /tmp/threat_profile_v92.data --stdio --children --percent-limit 2.0 -g none
```

### 実行結果

| Children | Self | 関数 | 分類 |
|---:|---:|---|---|
| 26.31% | 0.41% | `compute_eval_context` | eval 全体 |
| **18.25%** | **11.30%** | `refresh_accumulator_with_cache` | **FT weights full refresh** |
| **15.38%** | 4.97% | `update_accumulator_with_cache` | **FT weights 差分更新** |
| 11.30% | 7.20% | `MovePicker::next_move` | 指し手選択 |
| **7.49%** | **5.81%** | `append_changed_threat_indices` | **Threat index 計算** |
| 5.98% | 5.95% | `attackers_to_occ` | 効き計算 |
| 5.94% | 2.07% | `refresh_perspective_with_cache` | FT weights refresh 内部 |
| 5.20% | 2.69% | `do_move_with_prefetch` | 局面更新 |
| 5.01% | 5.00% | `compute_progress8kpabs_sum` | progress bucket 計算 |
| 4.49% | 0.09% | `mate_1ply` | 1手詰め |
| 4.40% | 2.27% | `__memset_avx2` | ゼロ埋め (libc) |
| 2.33% | 2.31% | `threat_features::attacks_from_piece` | Threat 効き |
| 2.05% | 2.05% | `HalfKA_hm::append_active_indices` | HalfKA index |
| 2.89% | 2.89% | `__memmove_avx` | コピー (libc) |

### 発見事項

#### (A) Threat 関連の真のコスト上限 = 7.49%

`append_changed_threat_indices` の **children 7.49%** が Threat 系の実質コスト上限。
`attacks_from_piece` (2.33%) も呼ばれているが、children に含まれるので
全体で見るとこれを二重カウントしない。

dims を半減 (cross-side) しても、理論的には Threat 関連コストの半分が削れるだけ:
- Threat children 7.49% × 0.5 = 3.7%
- 実測 ipn -5.88% (他の軽微な副次効果を含む)

**結論**: dims 削減で削れる探索時間の上限は **7.5% 程度**。これを超えるインパクトは
dims 削減からは得られない。今後の dims 削減実験は最大でもこの天井を意識する。

#### (B) FT weights accumulator 管理が合計 33.6% で支配的

- `refresh_accumulator_with_cache` children **18.25%**
- `update_accumulator_with_cache` children **15.38%**
- 合計 **33.6%** が FT weights の行操作 (sub/add on L0 × active features)

この中には **HalfKA_hm と Threat の両方の active feature** の差分適用が入っている。
Threat テーブルへの直接アクセスではなく、**FT weights (L0 × dims) への順次アクセス**が
cache pressure の主要経路。

これが「L3 境界跨ぎの単独効果が NPS +2-3%」程度に留まった理由でもある。
cache pressure は Threat テーブルではなく、FT weights の loop で決まる。

#### (C) refresh の比率が update より大きい — 要調査

通常、差分更新 (update) が効いていれば refresh はまれにしか呼ばれないはず。
しかし本計測では:

- refresh children 18.25% > update children 15.38%

refresh の頻度が高いか、1 回あたりのコストが重いか、のどちらか。**差分更新の cache
ヒット率に改善余地がある可能性**が高い。

#### (D) `compute_progress8kpabs_sum` 5.00% は別ルートの最適化余地

progress bucket 計算が 5% 占有。v87 は `nnue-progress-diff` feature で差分更新化
されており、このコストが削減されている。v92/v93/v94 は L1=768 以下で退行するため
無効 (`docs/feature_nnue_progress_diff.md` 参照)。**L1 サイズ依存**で有効化条件が
変わるので、L1=512 用に再設計する余地がある。

### 実験方針への影響

以下の優先順位で次の実験を進める:

1. **最優先: refresh_accumulator の頻度と cache ヒット率を実測**
   - refresh が 18.25% もある原因を特定
   - `nnue-stats` feature で refresh/update の呼び出し回数を取得
   - 差分更新に切り替えられる余地があれば、それだけで **NPS +5〜9% 相当** の期待値

2. **第 2 優先: FT weights sub/add の SIMD 化**
   - `refresh_accumulator_with_cache` self 11.30% と `update_accumulator_with_cache` self 4.97%
   - L0 loop の AVX2 ベクトル化の現状を確認
   - 最適化で self コストを半減できれば NPS +6〜8% 相当

3. **第 3 優先: Threat の `attacks_from_piece` と `append_changed_threat_indices` の高速化**
   - children 7.49% のうち、dims 非依存の attacks 計算部分が半分程度を占めると想定
   - bitboard 操作の SIMD 化、lookup table のレイアウト最適化
   - 最適化で半減できれば NPS +2〜4% 相当

4. **dims 削減実験は天井が見えている**
   - Threat 関連の実質コスト上限が 7.5% なので、dims 半減でも NPS +3〜4% が限度
   - 棋力低下とのトレードオフを考えると、**dims 削減単独の実験は優先度を下げる**
   - 中間 dims 設計 (実験 D) は前述の最適化と並行で考える

## nnue-stats による refresh/update 頻度と cache hit/miss 実測

perf profile で `refresh_accumulator_with_cache` 18.25% vs `update_accumulator_with_cache` 15.38%
という逆転（通常 refresh は rare のはず）が観測されたため、`nnue-stats` feature を拡張して
refresh/update の発動頻度と Finny Tables の cache hit/miss を実測した。

### 拡張したカウンタ

`feature_transformer_layer_stacks.rs` の `update_accumulator_with_cache` と
`refresh_accumulator_with_cache` に `count_refresh!` / `count_update!` を追加。
`accumulator_layer_stacks.rs` の `refresh_or_cache` に `count_cache_hit!` / `count_cache_miss!` を追加。

build feature: `layerstack-only,layerstacks-512,nnue-threat,nnue-stats`

### 結果

| 局面 | total updates | refresh | update | refresh 率 | cache hit | cache miss | hit 率 |
|---|---:|---:|---:|---:|---:|---:|---:|
| hirate-like (序盤風) | 7,110,628 | 649,871 | 6,460,757 | 9.1% | 662,011 | 71 | **99.99%** |
| complex-middle (中盤) | 5,822,880 | 2,223,180 | 3,599,700 | **38.2%** | 2,299,485 | 125 | **99.99%** |
| tactical (戦術) | 5,113,528 | 2,187,507 | 2,926,021 | **42.8%** | 2,208,721 | 130 | **99.99%** |
| movegen-heavy (複雑) | 6,454,828 | 2,094,039 | 4,360,789 | **32.4%** | 2,047,318 | 96 | **100.00%** |

### 発見事項

#### (E) refresh 率が異常に高い (中盤 30-43%)

`needs_refresh` の実装:
```rust
fn needs_refresh(dirty_piece: &DirtyPiece, perspective: Color) -> bool {
    dirty_piece.king_moved[perspective.index()]
}
```

→ **玉が動いたら常に refresh path**。HalfKA_hm の king_bucket は玉位置の
(file_m * 9 + rank) で 45 通り。玉が 1 マスでも動けば king_bucket が変わり、
全 feature index が `PIECE_INPUTS` 単位でシフトするため差分更新不可。

#### (F) cache は 99.99% hit している

cache miss は局面あたり 100 程度のみ（初回到達の king_sq での full rebuild）。
**全 refresh の実質 100% が cache hit 経由で処理されている**。

**つまり改善余地は cache miss 削減ではなく、cache hit 時の処理そのものの高速化**。

#### (F') 差分駒数ヒストグラム

cache hit 時に cache と現在の間で実際に add/sub される「差分駒数」を計測するため、
`symmetric_diff_count` を追加して histogram を取得した。

| 局面 | **平均** | 0 | 1-2 | 3-5 | 6-10 | 11-20 | 21-40 | 41+ |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| hirate-like | **8.10** | 2.3% | 4.8% | 30.5% | 37.9% | 21.8% | 2.7% | 0.0% |
| complex-middle | **6.93** | 2.7% | 8.4% | 25.0% | 49.2% | 14.2% | 0.6% | 0.0% |
| tactical | **6.63** | 4.3% | 9.8% | 27.8% | 44.3% | 13.2% | 0.6% | 0.0% |
| movegen-heavy | **5.98** | 5.0% | 11.9% | 30.0% | 43.1% | 9.9% | 0.1% | 0.0% |

**平均差分駒数 ≈ 6-8 個**、80% が 3-10 個の範囲、41 個以上はほぼゼロ。

#### (F'') 重要な気付き: Finny Tables は既に差分駆動として正しく機能している

当初「玉移動で king_bucket が変わる → 全 feature index が変わる → 全 80 個 apply」
と想定していたが、実測は **平均 6-8 個**。これは cache key が `king_sq` (81) なので、
「同じ king_sq に戻ってきたら前回との差分だけ」が正しく働いている証拠。

- active 数 40 個のうち、平均 6-8 個が変化
- 残り 32-34 個は cached と current で一致 → add/sub 不要

つまり「king_bucket 変更 → 全 feature 再計算」という当初の推測は間違いで、
**rshogi の refresh path は既に Stockfish と同じ差分駆動動作をしている**。

### refresh のコスト構造モデル

`refresh_accumulator_with_cache` self 11.30% の内訳を以下のモデルで近似:

- **fixed overhead**: `append_active_indices` (40 ops) + `sort_unstable` (~200 ops) + `apply_diff` スキャン (~80 ops) = **~320 ops**
- **差分 apply 本体**: 平均 7 個 × L0 loop (L0=512, 32 vec ops) = **~224 vec ops**
- 合計 per-call: ~544 ops 相当

update path (per-call): 差分 2-3 個 × 32 vec ops = **~80 ops 相当**

refresh per-call / update per-call ≒ 544 / 80 ≒ **6.8 倍** (profile 上の self 比 5.3 倍と整合)

### 改修の期待効果

fixed overhead 320 ops を消すと:
- refresh per-call: 544 → 224 (**41%**)
- refresh self 11.30% × 0.41 ≒ **4.6%**
- 削減: 11.30% - 4.6% = **6.7%** → **NPS +7% 相当**

ただし上限値。実際の削減量は:
- `symmetric_diff_count` / `pieces` 比較にも同程度の per-call コストがかかる可能性
- 差分 apply 本体の割合が想定より大きい場合、fixed overhead 削減の効果は相対的に小さくなる

#### (G) cache hit 時の処理が重い

rshogi の `refresh_perspective_with_cache` は:
1. `append_active_indices` で全 active feature を再集計 — **O(MAX_ACTIVE ≒ 数百)**
2. `sort_unstable` — **O(n log n)**
3. `refresh_or_cache` 内で sorted 対称差 + add/sub — **O(n)**

これが perf profile の `refresh_accumulator_with_cache` self 11.30% の正体。

## Stockfish Finny Tables 実装との比較

`/mnt/nvme1/development/Stockfish/src/nnue/nnue_accumulator.{h,cpp}` の実装を確認。

### Stockfish の cache entry 構造 (`nnue_accumulator.h:69`)

```cpp
struct alignas(CacheLineSize) Entry {
    std::array<BiasType, Size>              accumulation;
    std::array<PSQTWeightType, PSQTBuckets> psqtAccumulation;
    std::array<Piece, SQUARE_NB>            pieces;   // ← 駒配置そのもの
    Bitboard                                pieceBB;  // ← 全駒 Bitboard
};
```

### Stockfish の refresh 処理 (`nnue_accumulator.cpp:696 update_accumulator_refresh_cache`)

```cpp
// 1. Bitboard XOR で差分抽出（AVX2 で 32 マスずつ SIMD 比較）
const Bitboard changedBB = get_changed_pieces(entry.pieces, pos.piece_array());
Bitboard removedBB = changedBB & entry.pieceBB;
Bitboard addedBB   = changedBB & pos.pieces();

// 2. 差分 square のみから feature index を作成
while (removedBB) {
    Square sq = pop_lsb(removedBB);
    removed.push_back(FeatureSet::make_index(perspective, sq, entry.pieces[sq], ksq));
}
while (addedBB) {
    Square sq = pop_lsb(addedBB);
    added.push_back(FeatureSet::make_index(perspective, sq, pos.piece_on(sq), ksq));
}

// 3. entry.accumulation から differential apply (SIMD vectorized)
```

### 本質的な差

| 項目 | rshogi | Stockfish |
|---|---|---|
| cache entry に保存 | **sorted active indices** | **駒配置 (pieces + pieceBB)** |
| hit 時の差分計算 | 全 active 再集計 + sort + 対称差 | Bitboard XOR + 差分 squares のみ |
| 計算量 | O(active 数 ≒ 数百) | **O(差分駒数 ≒ 1-10)** |
| SIMD 化 | なし (append_active と sort 部) | AVX2 `get_changed_pieces` |

差分駒数は通常 1-10 個 (cache 時点から現在までの駒移動数)。rshogi の「数百」と比べて
**1〜2 桁軽い**。これが 1 refresh あたりのコスト差の源泉と思われる。

## 改修 PoC の設計方針

### 必要な変更

1. **`AccCacheEntry` 構造**:
   - 既存: `accumulation` + `active_indices[MAX_ACTIVE]` + `num_active`
   - 新: `accumulation` + `pieces[SQUARE_NB]` + `piece_bb: Bitboard`
   - サイズ影響: MAX_ACTIVE ≒ 数百 × 4 bytes → SQUARE_NB = 81 × 1 byte + Bitboard 16 bytes ≒ **減る**

2. **`refresh_or_cache` の処理**:
   - `&[u32] sorted active` を受け取る → `&Position` を受け取る
   - 内部で `get_changed_pieces(entry.pieces, pos.piece_array())` で差分抽出
   - 差分 square から HalfKA_hm feature index を算出 (`halfka_index(king_bucket, pack_bonapiece)`)

3. **`get_changed_pieces` の実装** (新規):
   - rshogi の `Position::piece_array()` と cache.pieces の SIMD 比較
   - 81 マスは 96 bytes でパディングして 32 bytes × 3 回の AVX2 比較
   - ビットマスクから `Bitboard` (rshogi 型) を構築

4. **持ち駒 (hand) の扱い** — 将棋特有の課題:
   - HalfKA_hm は盤上駒だけでなく持ち駒も feature index に含む
   - Stockfish は盤上駒のみなので `pieces[]` と `pieceBB` で完結
   - rshogi では cache entry に持ち駒配列も追加する必要
   - 持ち駒差分: `hand[PIECE_TYPE × 2]` の比較で検出

5. **`feature_transformer_layer_stacks.rs::refresh_perspective_with_cache` 書き換え**:
   - `append_active_indices` の呼び出し削除
   - sort 削除
   - 新 API に位置情報を渡す

### 期待効果

- `refresh_perspective_with_cache` の per-call コストが `append_active_indices` (数百) → Bitboard 差分 (1-10) に
- perf profile で refresh 関連 ~13% のうち、active 集計・sort 分 (~10%) が削減
- **NPS +10% 相当の改善** (1 refresh の per-call コストが update path 並になる想定)

### 実装コスト

- 変更ファイル: `accumulator_layer_stacks.rs`, `feature_transformer_layer_stacks.rs`
- 新規コード: `get_changed_pieces`, 持ち駒差分ヘルパー
- テスト: 既存の accumulator テストを流用 + PoC 用の hit/miss 計測
- 規模: 200-300 行の改修 + テスト

### 段階的実装案

1. **Step 1**: cache entry 構造だけ Stockfish 風に書き換え (pieces + piece_bb + hand)
2. **Step 2**: refresh_perspective_with_cache の書き換え
3. **Step 3**: ベンチで cycles/node と refresh self 時間を比較
4. **Step 4**: 回帰チェック (Golden Forward、YO alignment)
5. **Step 5**: マージ判断

## refresh 経路分離計測 (2026-04-12 追加)

当初「refresh の 12% は玉移動 (`update_accumulator_with_cache` の reset path)
による Threat full refresh」と仮定して Threat Finny Tables 導入を検討していたが、
refresh の発生元を 2 経路に分けて計測したところ、**想定が完全に覆る結果**が出た。

### 計測方法

`stats.rs` に `refresh_full_count` / `refresh_reset_count` を追加:

- `count_refresh_full!()`: `refresh_accumulator_with_cache` の loop 内
  (LayerStacks の `do_update!` で prev が未計算のときに呼ばれる経路)
- `count_refresh_reset!()`: `update_accumulator_with_cache` 内で
  `HalfKA_hm_FeatureSet::needs_refresh` (king_moved) が true のときの経路

どちらも既存の `count_refresh!()` に加えて並行カウント。

### 結果 (v92, complex-middle, movetime 10s)

```
refresh:            2,176,100 (38.1%)
  full (stack):     2,127,900 (97.8%)  ← refresh_accumulator_with_cache 経由
  reset (king mv):     48,200 ( 2.2%)  ← update の玉移動経由
```

**refresh の 97.8% が `refresh_accumulator_with_cache` 経由で、update の reset path
(玉移動由来) は全体のわずか 2.2%**。

### 原因: pruning 後の accumulator chain 途切れ

LayerStacks の `update_accumulator` (`network_layer_stacks.rs::do_update!`) の
ロジック:

```rust
if current.computed_accumulation { return; }  // 既に計算済み
if let Some(prev_idx) = current.previous {
    if prev.computed_accumulation {
        // update 経路（差分更新）
    }
}
// update しなかった場合のフォールバック
$net.refresh_accumulator_with_cache(...);  // ← ここが self 12% の正体
```

つまり **prev が computed でないケースで必ず `refresh_accumulator_with_cache` に
フォールバック** する。

典型的な発生パターン:
1. do_move → push (entry[N+1] 未計算、prev=N)
2. pruning で eval せず即 undo_move → pop (current=N)
3. 別の手 do_move → push (entry[N+1] 上書き、prev=N)
4. eval を試みる → **entry[N+1] の accumulator は前の branch の残骸で未計算**
5. prev=N が computed なら update 経路で済むが、`current.computed_accumulation`
   自体は push 時に false 化されるので、勝手に computed にはならない

**ただし上記は勘違いで、実際には push 時の entry[N+1] は computed=false、prev=N は
computed=true のはず**。それでも refresh_full が 97.8% というのは別の原因がある。

候補:
- pruning で chain の中間 entry の eval が発生せず、遠い ancestor だけが computed
- `compute_eval_context` が呼ばれないノード (in_check 時の static_eval 継承など)
- quiescence search での jump で stack の整合性が崩れる

### Stockfish の対策 (find_usable_accumulator)

Stockfish は `find_usable_accumulator` を実装しており、prev が未計算の場合に
**さらに上位 ancestor を遡って computed な accumulator を探す** 仕組みがある。
見つかったら、そこから current までの dirty_piece chain を forward で差分累積
(`update_accumulator_incremental<Forward>`) する。逆方向にも対応。

rshogi にはこの機構がなく、prev 未計算 → 即 refresh にフォールバックしている。

### 改修の優先順位 (見直し)

先の PoC (HalfKA_hm の refresh_perspective_with_cache PieceList 化) と
Threat Finny Tables 導入の前に、**更に上流の問題がある**:

1. **最優先**: Stockfish 風 `find_usable_accumulator` の実装
   - ancestor を遡って computed な entry を見つける
   - forward または backward で dirty_piece chain を差分累積
   - 期待効果: refresh 発生率 38% → 数% に削減、NPS +10〜20% 相当
2. **次**: Threat Finny Tables (refresh 頻度が下がっても full refresh を更に減らす)
3. **後回し**: refresh_perspective_with_cache の PieceList 最適化 (効果 0% 確認済み)

## find_usable_accumulator 活用 PoC (2026-04-12)

既存の `find_usable_accumulator` + `forward_update_incremental` は実装済み
だったが、`network_layer_stacks.rs::do_update!` から呼ばれていなかった。
これを段階的フォールバック (1-step → find_usable + forward → refresh) に
改修した (`7ea08166`, `82f54b03`)。

### nnue-stats (profiling build, complex-middle)

```
refresh 率     : 38.1% → 29.5% (-22.6 pp)
refresh_full   : 2,127,900 → 1,521,898 (-28%)
forward_update : 0 → 287,524 (5.4%)
incremental率  : 61.9% → 70.5% (+8.6 pp)
```

**設計通り refresh を 28% 削減**。forward_update_incremental 経由で
ancestor からの chain 差分更新に置き換わった。

### production build での NPS 計測 (search_only_ab, 4局面, abba × 2 rounds)

旧 v92 (`c5e4e057`, 改修前) vs 新 v92 (`82f54b03`, find_usable + clone 削除):

| 項目 | baseline (c5e4e057) | candidate (82f54b03) | Δ |
|---|---:|---:|---:|
| avg_nps | 462,145 | 462,182 | **+0.01%** |
| cycles/node | 9,502.5 | 9,501.5 | −0.01% |
| instructions/node | 19,650.1 | 19,385.0 | **−1.35%** |
| CPI | 0.4835 | 0.4902 | +1.4% |

**production build では NPS 変化なし (+0.01% = 誤差)**。

### 中間経過: 7ea08166 単独の計測

`forward_update_incremental` の `source_acc.clone()` を残したままの
`7ea08166` 時点では NPS **-0.91%** まで退行していた:

| 項目 | baseline (c5e4e057) | candidate (7ea08166) | Δ |
|---|---:|---:|---:|
| avg_nps | 458,674 | 454,490 | -0.91% |
| cycles/node | 9,576.7 | 9,655.8 | +0.83% |
| instructions/node | 19,645.6 | 19,384.3 | -1.33% |

退行の主因は `source_acc.clone()` による 2 段 memcpy
(source → stack clone → current)。`82f54b03` で
`split_at_mut` 経由の直接コピーに変更したところ、退行は解消したが
NPS 改善には至らず誤差範囲に収束した。

### 考察

- **命令数削減は実測** (instructions/node -1.35%): refresh → forward_update 置換の計算量削減効果は発揮されている
- **CPI が +1.4% 悪化**: 関数呼び出し overhead、branch mispredict、または
  multi-step chain の cache 動作の違いにより、削減した命令数分の cycles が
  他の stall で相殺されている
- **profiling build で観測した +4.4% は LTO なしの副作用**: LTO なし
  では既存 1-step update path が遅く、改修効果が相対的に大きく見えた。
  LTO=fat の production build では既存 path が十分高速で、新 path の
  overhead が勝ってしまう

### 判断: 改修を revert

NPS 改善なし (+0.01% = 誤差) で、CLAUDE.md の「早すぎる最適化禁止・
測定なしの最適化禁止」方針に従い本改修は revert する。計測結果は
docs に残し、将来の改良の足場 (特に CPI 悪化の原因特定、
forward_update_incremental の更なる最適化) とする。

## nnue-progress-diff feature 再評価 (2026-04-12)

perf profile で `compute_progress8kpabs_sum` が self 5.00% と大きい。
過去の記録 (`memory/feature_nnue_progress_diff.md`) では
「L1=1536 で有効、L1=768 で退行」とあり L1=512 は未評価だったため、
v92 (L1=512 + profile 0 + Threat) で再測定した。

### 計測 (search_only_ab, 4 局面, abba × 2 rounds, production build)

```bash
cargo build --profile production -p rshogi-usi --bin rshogi-usi \
  --features layerstack-only,layerstacks-512,nnue-threat,nnue-progress-diff
```

| 項目 | baseline (nnue-progress-diff なし) | candidate (あり) | Δ |
|---|---:|---:|---:|
| avg_nps | 451,128 | 464,604 | **+2.99%** |
| cycles/node | 9,726.4 | 9,444.4 | **-2.90%** |
| instructions/node | 19,843.2 | 19,111.4 | **-3.69%** |
| CPI | 0.4900 | 0.4941 | +0.8% |

**NPS +2.99% の確実な改善**。instructions/node の -3.69% 削減が主で、
CPI の微増 (+0.8%) を上回る。

### 過去評価との差異

過去 (`feature_nnue_progress_diff.md`):
- v87 (L1=1536, no Threat): cycles **-3〜-4%** (有効)
- v91 (L1=768, Threat あり): cycles **+2〜+6%** (退行)
- L1=512 は未評価

今回:
- v92 (L1=512, profile 0 + Threat): **NPS +2.99%** (cycles -2.90%, 有効)

**L1=512 では L1=768 と異なり改善効果が出る**。L1 が小さいほど accumulator
エントリが小さく、`progress_sum`/`computed_progress` field touch の
cache pressure が相対的に小さいため、差分更新の命令節約が活きる。

### 推奨 build feature (v92 系)

```bash
# 旧 (nnue-progress-diff なし)
cargo build --profile production -p rshogi-usi --bin rshogi-usi \
  --features layerstack-only,layerstacks-512,nnue-threat

# 新 (nnue-progress-diff 追加)  ← NPS +2.99%
cargo build --profile production -p rshogi-usi --bin rshogi-usi \
  --features layerstack-only,layerstacks-512,nnue-threat,nnue-progress-diff
```

**コード変更ゼロ**、feature flag の追加のみで +2.99% が得られる。
運用バイナリ (v92, v94 等 L1=512 モデル) は今後 `nnue-progress-diff` を
有効にするのが推奨。

L1=768 (v91, v93) では退行するため、必ず L1 サイズごとに
build features を切り替える必要がある。

## 実験方針への示唆

1. **refresh_accumulator の頻度と cache ヒット率の実測が最優先** — NPS +5〜9% 相当の余地
2. **FT weights sub/add の SIMD 最適化が第 2 優先** — NPS +6〜8% 相当の余地
3. **Threat 関連の純粋コスト上限は 7.5%** — dims 削減実験の天井
4. **instructions/node 削減のほうが支配的** — Threat accumulate の計算コストが NPS への主要ボトルネック
5. **テーブルサイズ削減による cache 関連の寄与は NPS +2〜4% 程度のオーダー**
   (「L3 以下にする単独効果」は同一 profile での L3 境界跨ぎ比較がないため本計測からは厳密には分離できない)
6. **避けるべき方向性**: cross-side のような極端な dims 削減。eval 品質低下のペナルティが cache 削減効果を上回る

---

## 参照

- `.claude/skills/usi-perf-measure/SKILL.md` — 計測手法の詳細
- `docs/performance/threat_table_size_reference.md` — Threat テーブルサイズ参照
- `crates/tools/src/bin/search_only_ab.rs` — 計測ツール実装
- JSON 結果 (CPI 分解): `/tmp/perf_measure_20260412/*.json`
- perf record data (hotspot): `/tmp/threat_profile_v92.data`
- ログ: `/tmp/perf_measure_20260412/*.log`

作成日: 2026-04-12
