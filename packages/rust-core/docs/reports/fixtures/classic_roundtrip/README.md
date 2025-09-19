# Classic Round-Trip Fixtures

このディレクトリには Classic NNUE ラウンドトリップ検証用の最小フィクスチャとスモーク実行スクリプトをまとめています。

- `train.jsonl` / `val.jsonl` — 64 サンプルの cp ラベル付きデータセット。`train_nnue --arch classic` を 1 エポック回すための軽量データです。
- `positions.sfen` — 64 局面（ply32 サンプル由来）の SFEN リスト。`verify_classic_roundtrip` で FP32/INT 差分を測定します。
- `run_smoke.sh` — 学習 → Classic export → round-trip 比較までを一括実行するスクリプト。`target/classic_roundtrip_smoke` 配下に成果物（`nn.fp32.bin` / `nn.classic.nnue` / `nn.classic.scales.json` / `roundtrip.json` / `worst.jsonl`）を残します。

## 使い方

```bash
# リリースビルドでスモーク
./docs/reports/fixtures/classic_roundtrip/run_smoke.sh

# デバッグビルドで試す場合（環境変数で切り替え）
CARGO_PROFILE=debug ./docs/reports/fixtures/classic_roundtrip/run_smoke.sh
```

追加の `cargo run` オプションを渡したい場合は、環境変数 `TRAIN_EXTRA_ARGS` / `VERIFY_EXTRA_ARGS` を利用してください。最初の実行時は Single アーキテクチャの教師ネットワークも自動生成され、以降は既存の `target/classic_roundtrip_smoke/single_teacher/nn.fp32.bin` を再利用します。

## ベースライン計測

より厳密なラウンドトリップ統計を取得したい場合は、同ディレクトリの `measure_roundtrip.py` を実行してください。既定では教師シード 2025 / Classic シード 42〜46 の 5 回分を計測し、結果を `target/classic_roundtrip_measure/baseline_roundtrip.json` および `docs/reports/fixtures/classic_roundtrip/baseline_roundtrip.json` に保存します。

```bash
cargo run -p tools --bin measure_classic_roundtrip -- --profile release
```

現状のサンプルデータ（64 局面）では、Classic FP32 と INT の差分は概ね次の分布になっています。

| 指標 | 最大値 | 平均値 |
| ---- | ------ | ------ |
| `max_abs` | 約 254 cp | 約 220 cp |
| `mean_abs` | 約 94 cp  | 約 80 cp |
| `p95_abs` | 約 214 cp | 約 184 cp |

本番 CI では実データに基づき余裕を持った閾値を設定してください。
