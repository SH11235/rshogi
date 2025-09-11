# Spec 013 — 最小ガントレット自動化

目的: 昇格/降格を自動判断する最小ガントレットの仕様を固定し、結果を機械可読にする。

## 固定条件
- 時間: `0/1+0.1`
- 対局数: `100–200`
- `threads=1`, `hash_mb=256`（CLI: `--hash-mb 256`）, `book=fixed-100.epd`
- MultiPV: `1`（PVスプレッド観測は別途 MultiPV=3 で測定）

## CLI（例）
```
cargo run -p tools --bin gauntlet -- \
  --base runs/baseline/nn.bin --cand runs/candidate/nn.bin \
  --time "0/1+0.1" --games 200 --threads 1 --hash-mb 256 \
  --book assets/opening/fixed-100.epd --multipv 1 \
  --json runs/gauntlet/out.json --report runs/gauntlet/report.md
```

備考:
- `--json -` または `--report -` を指定すると、対応する出力を STDOUT に書き出します。
  - その場合、構造化ログ（structured_v1）は STDERR に出力されます（混在防止）。
- `--seed <N>` を指定すると、開幕順（ペアリング済みの2局セット）を N で決定的にシャッフルします（既定は非シャッフル）。

## 出力スキーマ
- JSON 出力は `docs/schemas/gauntlet_out.schema.json` に準拠
- 含むべき情報
  - `env`: CPU, rustc, commit, toolchain
  - `params`: time, games, threads, hash_mb, book, multipv
  - `summary`: winrate, draw, nps_delta_pct, pv_spread_p90_cp, gate
    - `winrate` は互換のため名称を保持しつつ、実体は Score rate（W=1, D=0.5, L=0）
  - `series`: 各対局の結果（先後/手数/勝敗/消費ノード/NPS）
  - 透明性向上のため、Summary に `nps_samples` と `pv_spread_samples` を任意で含みます（後方互換）

## Gate 判定
- 準合格（provisional）: 決着局（W/L）の勝率について、Wilson 区間95%の下限が 50% を超える場合
- 最終合格（pass）: スコア率（W=1, D=0.5, L=0）が 55%以上 かつ NPS ±3% 以内
- 代表/アンチの2系統ブック
  - 昇格判定は代表系で実施
  - 退避評価はアンチ系でも確認（オーバーフィット抑止）

## ロールバック
- 失敗時は重みを昇格させない（自動的にベースライン維持）
- 出力 `summary.gate = "reject"` とし、理由を `summary.reject_reason` に記載

## Fixtures
- 代表ブック: `docs/reports/fixtures/opening/representative.epd`
- アンチブック: `docs/reports/fixtures/opening/anti.epd`
- 使用例（代表ブック）:
  ```sh
  cargo run -p tools --bin gauntlet -- \
    --base runs/baseline/nn.bin --cand runs/candidate/nn.bin \
    --time "0/1+0.1" --games 200 --threads 1 --hash-mb 256 \
    --book docs/reports/fixtures/opening/representative.epd --multipv 1 \
    --json runs/gauntlet/out.json --report runs/gauntlet/report.md
  ```

## 実装メモ（計測の厳密化）
- NPS: サンプルごとに TT をクリアしてから固定時間探索し、`f64` で累積平均化（丸め誤差抑制）
- PV スプレッド: MultiPV=3 の root を用い、mate スコアを含むサンプルは除外（閾値: `|cp| ≥ 30000`）
- 環境情報（env）はクロスプラットフォームな手段で収集（Linux: /proc/cpuinfo, macOS: sysctl, Windows: PowerShell CIM）。取得不可時は "unknown" とする。
