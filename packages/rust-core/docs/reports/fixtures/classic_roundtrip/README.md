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
