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

## 出力スキーマ
- JSON 出力は `docs/schemas/gauntlet_out.schema.json` に準拠
- 含むべき情報
  - `env`: CPU, rustc, commit, toolchain
  - `params`: time, games, threads, hash_mb, book, multipv
  - `summary`: winrate, draw, nps_delta_pct, pv_spread_p90_cp, gate
  - `series`: 各対局の結果（先後/手数/勝敗/消費ノード/NPS）

## Gate 判定
- 勝率: Wilson区間95%の下限 > 50% を準合格
- 最終合格: 勝率 +5%pt 以上 かつ NPS ±3% 以内
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
