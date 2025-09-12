# ガントレット後フォローアップ計画（次のアクション）

本書は、直近のガントレット実行結果を踏まえた「次のアクション」を短期～中期の計画としてまとめます。測定条件・Gate は `docs/00_charter.md` に準拠し、実行フローは `docs/guides/pipeline_walkthrough.md` に従います。

## 現状の定量サマリ（代表100, 200局, seed=42, block, pv_ms=500）
- Score rate: 50.2%（W:18 / L:17 / D:165, draw=82.5%）
- NPS delta: +1.51%（Gate条件 ±3%以内を満たす）
- PV spread p90: 1200 cp（PV サンプル 53/100）
- 判定: Gate=reject（理由: score<55%）

参考（レンジ推定）:
- 200局・引分82.5%条件での 1局あたりスコアの95%CIは概ね 50.2% ± 3.0% → 55%には統計的に届かないレンジ。
- 55%到達に必要な上積み: Score +9.5pt（勝ち越し+10相当） / Elo目安 +35。

結論: 測定系（NPS±3%以内/TTクリア/出力ロック等）は安定。阻害要因は“強さ（Elo不足）”。

## 優先順位と実行項目

### A. 候補モデルの再学習／設定調整（最優先: +35 Elo を狙う）
1) データで押す（Hard Mining + 分布整理）
- 代表/アンチの両ブックで「評価が割れる・手が入れ替わる」局面を常時回収→再注釈（multipv=2〜3, 長思考）
- 序・中・終盤の分布を均し、詰み境界・王手絡みの比率を上げる
- 既存データの重複/近縁を抑えて有効密度を上げる

2) 重み付けで押す（`docs/specs/012_weighting.md`）
- 例: `--weighting exact --weighting phase --weighting mate`
- 初期係数目安: `--w-exact 1.1〜1.3` / `--w-phase-endgame 1.2〜1.4` / `--w-mate-ring 1.1〜1.3`
- 目的: 決着寄与領域（終盤/詰み縁/反転点）の精度向上→引分多めでも勝ち切りを押し上げる

3) LR スケジュール最適化（`docs/specs/011_lr_schedule.md`）
- 起点: `--lr-schedule cosine --lr-warmup-epochs 1`
- 末期の過学習抑止: Plateau 併用（`--lr-plateau-patience > 0`）

目標: 「+35 Elo」。データ×重み付け×LR で +20〜50 Elo の押上げは現実的。

### B. 測定の補助（本筋ではないが解像度を上げる）
- PV サンプル充足: `--pv-ms 700〜1000` で 90/100〜100/100 を目指す（Gate合否には非連動）。
- 分散確認の追試: 200→400 局で標準誤差を √2 に低減（平均が50%付近なら合否は変わらない見込み）。

## 実行コマンド雛形（例）

### Hard Mining 抽出（例）
```bash
cargo run -p tools --bin orchestrate_ambiguous -- \
  -i runs/train.jsonl -o runs/hard_mine.jsonl \
  --gate docs/specs/012_weighting.md --report exact-rate gap-distribution \
  --multipv 3 --time "0/2+0.2"
```

### 学習（重み付けとLR）
```bash
cargo run -p tools --bin train_nnue -- \
  -i runs/train.cache.gz -v runs/val.jsonl \
  -e 3 -b 16384 \
  --lr-schedule cosine --lr-warmup-epochs 1 --lr-plateau-patience 2 \
  --weighting exact --weighting phase --weighting mate \
  --w-exact 1.2 --w-phase-endgame 1.3 --w-mate-ring 1.2 \
  --structured-log runs/train_structured.jsonl
```

### ガントレット（代表/アンチ）
```bash
# 代表
RAYON_NUM_THREADS=1 target/release/gauntlet \
  --base runs/nnue_local/nn_best.fp32.bin --cand runs/nnue_*/nn.fp32.bin \
  --time "0/1+0.1" --games 200 --threads 1 --hash-mb 256 \
  --book docs/reports/fixtures/opening/representative_100.epd --multipv 1 \
  --json runs/gauntlet/out.json --report runs/gauntlet/report.md \
  --seed 42 --seed-mode block --pv-ms 700

# アンチ（退避確認）
RAYON_NUM_THREADS=1 target/release/gauntlet \
  --base runs/nnue_local/nn_best.fp32.bin --cand runs/nnue_*/nn.fp32.bin \
  --time "0/1+0.1" --games 200 --threads 1 --hash-mb 256 \
  --book docs/reports/fixtures/opening/anti.epd --multipv 1 \
  --json runs/gauntlet/out_anti.json --report runs/gauntlet/report_anti.md \
  --seed 42 --seed-mode block --pv-ms 700
```

## 成功基準（Definition of Done）
- Score rate ≥ 55% かつ |NPS delta| ≤ 3%
- 代表/アンチ両系で退避確認に問題なし
- 訓練ログ/集計は structured_v1 と gauntlet_out schema に準拠し `docs/reports` へ保存

## `docs/20_engine.md` との整合
本計画は `20_engine.md` の以下と一致します。
- B-1「教師データ拡大」/ B-3「Hard Mining」: データ主導の強化サイクル
- 「学習レジメン更新（LR/重み付け）」: LRスケジュール最適化・重み付け導入
- Phase 1 の目的（NPS × 表現力の土台）と、ガントレット Gate による昇格運用

> したがって、候補モデルの再学習/設定調整で勝率を引き上げる方針は `20_engine.md` の作業方針と合致しています。

## メモ
- `--pv-ms` は Gate 合否には直接影響せず、分析の安定化（PV散布の“見える化”）に寄与します。
- 代表ブックは `docs/reports/fixtures/opening/representative_100.epd` を固定（先頭100がNPS/PV）。必要に応じて再抽出可。

---

## 付録: 再学習後の PV 安定計測レシピ（pv_ms を上げた一括計測）

目的: 再学習後に PV スプレッドのサンプル数と p90 を安定取得（≧90/100 推奨）し、前後比較を容易にする。

前提:
- 固定条件は `docs/00_charter.md` に従う（threads=1, hash_mb=256, time="0/1+0.1"）。
- 代表100ブック: `docs/reports/fixtures/opening/representative_100.epd`（先頭100でNPS/PVサンプリング）。
- 再現性のため `--seed 42 --seed-mode block` を推奨。

推奨パラメータ:
- `--pv-ms 700〜1000`（最低100msにクランプ。NPSには影響しない＝対局は従来どおり）

例（タイムスタンプ付きディレクトリに格納）:
```bash
ts=$(date +%Y%m%d-%H%M%S)
outdir=runs/gauntlet/$ts
mkdir -p "$outdir"

cargo build -p tools --release

# 代表ブックで計測（pv_ms=1000 推奨）
RAYON_NUM_THREADS=1 target/release/gauntlet \
  --base runs/nnue_local/nn_best.fp32.bin \
  --cand runs/nnue_*/nn.fp32.bin \
  --time "0/1+0.1" --games 200 --threads 1 --hash-mb 256 \
  --book docs/reports/fixtures/opening/representative_100.epd --multipv 1 \
  --json "$outdir/out.json" --report "$outdir/report.md" \
  --seed 42 --seed-mode block --pv-ms 1000 \
  > "$outdir/structured.jsonl"

# （任意）アンチブックでも追試
RAYON_NUM_THREADS=1 target/release/gauntlet \
  --base runs/nnue_local/nn_best.fp32.bin \
  --cand runs/nnue_*/nn.fp32.bin \
  --time "0/1+0.1" --games 200 --threads 1 --hash-mb 256 \
  --book docs/reports/fixtures/opening/anti.epd --multipv 1 \
  --json "$outdir/out_anti.json" --report "$outdir/report_anti.md" \
  --seed 42 --seed-mode block --pv-ms 1000 \
  > "$outdir/structured_anti.jsonl"
```

ログの取り回し（集計のひな形）:
```bash
# 代表の PV 指標を TSV に追記（p90 とサンプル数）
jq -r '[.summary.pv_spread_p90_cp, .summary.pv_spread_samples] | @tsv' \
  "$outdir/out.json" >> "$outdir/pv_metrics.tsv"

# 代表の合否サマリ（score, draw, nps_delta, gate）を TSV に追記
jq -r '[.summary.winrate, .summary.draw, .summary.nps_delta_pct, .summary.gate] | @tsv' \
  "$outdir/out.json" >> "$outdir/summary.tsv"

# 構造化ログ（structured_v1）は JSONL 1行。必要に応じて集約へ追記
cat "$outdir/structured.jsonl" >> runs/gauntlet/structured_history.jsonl
```

注意:
- `--json -` や `--report -` を使うと、それらが STDOUT へ出力され、structured_v1 は STDERR に切り替わります（混在防止）。本レシピではファイル出力にして structured を STDOUT リダイレクト（`>`）しています。
- `--pv-ms` は合否に直接影響せず、分析（PV の散らばり）安定化のための設定です。勝率/NPS は従来条件で評価されます。
