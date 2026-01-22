# eval/ - NNUEファイル配置ディレクトリ

このディレクトリはgit管理外。NNUEファイル（*.nnue, *.bin）を配置する。

## ディレクトリ命名規則

`{feature}_{L1}x2-{L2}-{L3}_{activation}/`

- `{feature}`: 特徴量タイプ（halfkp, halfka_hm）
- `{L1}x2-{L2}-{L3}`: ネットワーク次元（L1はperspective毎）
- `{activation}`: 活性化関数（必須）

### 活性化関数サフィックス

| サフィックス | 活性化関数 | 説明 |
|-------------|-----------|------|
| `_crelu` | CReLU | clamp(0, 127) |
| `_crelu_pairwise` | CReLU + Pairwise | CReLU後に隣接要素ペアの積（Stockfish方式） |
| `_screlu` | SCReLU | Squared CReLU |

### 例

| ディレクトリ | 特徴量 | L1 | L2 | L3 | 活性化 |
|-------------|--------|-----|-----|-----|--------|
| halfkp_256x2-32-32_crelu/ | HalfKP | 256 | 32 | 32 | CReLU |
| halfkp_256x2-32-32_crelu_pairwise/ | HalfKP | 256 | 32 | 32 | CReLU+Pairwise |
| halfka_hm_256x2-32-32_crelu/ | HalfKA_hm | 256 | 32 | 32 | CReLU |
| halfka_hm_512x2-8-96_crelu/ | HalfKA_hm | 512 | 8 | 96 | CReLU |
| halfka_hm_512x2-8-96_crelu_pairwise/ | HalfKA_hm | 512 | 8 | 96 | CReLU+Pairwise |
| halfka_hm_1024x2-8-96_crelu/ | HalfKA_hm | 1024 | 8 | 96 | CReLU |

## ベンチマーク実行例

```bash
./target/release/bench_nnue_eval \
  --nnue-file eval/halfkp_256x2-32-32_crelu/<FILE>.nnue
```
