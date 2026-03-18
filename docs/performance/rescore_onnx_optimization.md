# rescore_pack ONNX 推論最適化の記録

## 概要

`rescore_pack` の ONNX 直接推論モード（`--onnx-model` / `--dlshogi-onnx-model`）の高速化調査。
73億レコード（299GB）の全量リスコアを現実的な時間で完了させることが目的。

- 環境: RTX 3080 Ti (12GB), 32コア CPU, ONNX Runtime 1.21.1 GPU, ort 2.0.0-rc.12
- 対象モデル: AobaZero ResNet30x384 (308MB), DL水匠 (55MB)
- 日付: 2026-03-19

## ボトルネック特定

### 初期仮説（誤り）

> バッチサイズを上げても速度が変わらない = GPU は遊んでおり、CPU がボトルネック

### 実測結果

フェーズ別計測を追加して各ステップの所要時間を計測した結果:

```
[GPU thread] batches=98, tensor=442.8µs, run=42.4s, extract=1.5ms
[main]       read=228.6ms, feat=491.6ms, recv+write=41.7s, to_vec+send=433.7ms
```

| フェーズ | 時間 (100K, batch=1024) | 割合 |
|---|---|---|
| session.run() (GPU推論) | 42.4s | 96.7% |
| 特徴量構築 (rayon並列) | 0.5s | 1.1% |
| to_vec + channel send | 0.4s | 1.0% |
| 読み込み + unpack_sfen | 0.2s | 0.5% |
| Tensor::from_array | 0.0004s | ~0% |

**GPU 推論（session.run()）が全体の 97% を占め、CPU は律速ではなかった。**

バッチサイズを変えても total GPU time は変わらない理由は、同じ計算量を分割しているだけで
GPU は常にフル稼働しているため。

### ORT プロファイル（session.run() 内訳）

`--ort-profile` オプションで ORT の Chrome trace JSON を取得:

| op | 時間 | 割合 (Node内) |
|---|---|---|
| Conv | 0.705s | 96.4% |
| Gemm | 0.017s | 2.3% |
| BatchNormalization | 0.004s | 0.5% |
| Copy/Transfer | 0s | 0% |

**H2D/D2H コピーは 0%。session.run() の時間は 100% CUDA kernel（主に Conv）。**

## 実施した最適化

### 採用（コミット済み）

1. **ストリーミング読み込み**
   - 全レコード事前ロード → バッチ単位ストリーム読み込み
   - メモリ: ファイルサイズ依存 → バッチサイズ依存（数MB）

2. **rayon 並列特徴量構築**
   - `into_par_iter().zip().zip().for_each()` で 32 コア並列化
   - 特徴量構築: ~20s → 0.5s（~40x 高速化）
   - Position::new() / set_sfen() にグローバルロックがないことを確認済み
     （isolated benchmark で 13.8x スケール確認）

3. **GPU パイプライン（ダブルバッファリング）**
   - `mpsc::sync_channel` で GPU スレッドを分離
   - バッチ N の GPU 推論中にバッチ N+1 の読み込み + 特徴量構築を並行実行

4. **AobaZero / dlshogi 共通化**
   - 470行 × 2 のほぼ同一関数 → 共通パイプライン関数 + クロージャで差異吸収（-97行）

5. **GraphOptimizationLevel::All**
   - Level1 → All に変更（ORT デフォルト相当）
   - 実測では Level1 と差なし（モデル構造的に追加最適化の余地なし）

6. **`--ort-profile` オプション追加**
   - session.run() 内訳を Chrome trace JSON で出力

### 試行したが効果なし

7. **GraphOptimizationLevel::Level3**
   - ort 2.0.0-rc.12 では `Level3` → `ORT_ENABLE_LAYOUT` にマッピングされる
   - ORT 1.21.1 で `graph_optimization_level is not valid` エラー
   - 正しくは `All` → `ORT_ENABLE_ALL` を使うべき（上記 5 で対応済み）

8. **TF32 (Tensor Float 32)**
   - `CUDAExecutionProvider::with_tf32(true)` を設定
   - AobaZero で session.run = 21.6s（TF32 なし: 21.8s）→ 差なし
   - メモリ帯域律速のモデルでは compute 精度を下げても効果なし

9. **NHWC レイアウト**
   - `CUDAExecutionProvider::with_prefer_nhwc(true)` を設定
   - session.run = 25.2s（NCHW: 21.8s）→ **逆に 16% 遅化**
   - NCHW → NHWC のレイアウト変換オーバーヘッドがテンソルコアの恩恵を上回る

10. **TF32 + NHWC + conv_max_workspace + fuse_conv_bias 全部入り**
    - 上記 8+9 の複合。NHWC の遅化が支配的で悪化

11. **TensorRT Execution Provider**
    - `TensorRTExecutionProvider::with_fp16(true).with_engine_cache(true)` を設定
    - CUDA EP をフォールバックとして登録
    - 結果: session.run = 21.5s（CUDA EP のみ: 21.8s）→ 差なし
    - TRT エンジンキャッシュが空のまま → TensorRT が実際には有効化されていない
    - ORT 1.21.1 の GPU ビルドに `libonnxruntime_providers_tensorrt.so` は存在するが、
      TRT 10.15 (cuda13.1) との互換性の問題で自動的に CUDA EP にフォールバックした模様
    - 詳細調査には ORT のビルドオプションや TRT バージョン互換性の確認が必要

### 評価したが不採用

12. **IoBinding + pinned host memory**
    - ORT ドキュメントが「毎回入力が変わるモデル」「CPU→GPU→CPU パイプライン」では
      効果なしと明記
    - ORT プロファイルで H2D/D2H コピーが 0% と確認済みのため優先度最低

13. **CUDA Graph**
    - IoBinding が前提、かつ固定バッチサイズ + パディングの実装が必要
    - ORT プロファイルで kernel launch overhead が支配的でないため効果見込みなし
    - コスト対効果が合わないため見送り

### 精度劣化を伴う施策（未採用）

14. **FP16 モデル変換（全体）**
    - `onnxconverter_common.float16.convert_float_to_float16(keep_io_types=True)` で変換
    - 速度: AobaZero 1.44x, DL水匠 1.41x
    - 精度:
      - **DL水匠: 実用的**（90.5% が ±10cp 以内、平均 5.1cp 差、最大 130cp）
      - **AobaZero: 不可**（12.8% しか完全一致せず、平均 344.9cp 差、最大 4,237cp）
    - AobaZero の精度劣化原因: BatchNorm の微小パラメータが FP16 で切り捨て
      （変換時に `truncated to 1e-07` の warning が大量発生）
    - 精度無劣化を前提とするため不採用

15. **Selective FP16（BatchNorm のみ FP32 維持）**
    - AobaZero の精度劣化を軽減する案
    - `op_block_list=["BatchNormalization"]` で BatchNorm のみ FP32 維持
    - 精度を犠牲にする最適化であるため未実施

## バッチサイズ測定

### AobaZero (50K records)

| batch | GPU run | real | pos/sec |
|---|---|---|---|
| 256 | 23.8s | 25.0s | 2,000 |
| 512 | 22.5s | 23.7s | 2,108 |
| **1024** | **21.8s** | **23.1s** | **2,168** |
| 2048 | 22.4s | 24.7s | 2,024 |
| 4096 | 22.1s | 24.5s | 2,041 |
| 8192 | 21.8s | 24.3s | 2,058 |

### DL水匠 (200K records)

| batch | GPU run | real | to_vec+send | pos/sec |
|---|---|---|---|---|
| 256 | 24.6s | 26.7s | 1.4s | 7,491 |
| **1024** | **23.0s** | **24.4s** | **0.8s** | **8,197** |
| 4096 | 21.7s | 26.5s | 4.1s | 7,547 |
| 8192 | 21.4s | 26.2s | 4.1s | 7,634 |

batch=1024 が両モデルでスイートスポット。大バッチでは `to_vec()` コピーが増加。

## 現状のスループットと全量推定

| モデル | pos/sec | 73億レコード推定 |
|---|---|---|
| AobaZero FP32 | ~2,200 | ~38日 |
| DL水匠 FP32 | ~8,200 | ~10日 |
| AobaZero FP16 (精度劣化あり) | ~3,200 | ~26日 |
| DL水匠 FP16 (実用的精度) | ~10,200 | ~8日 |

## 今後の選択肢

精度無劣化の前提では単一 GPU での高速化は限界に到達済み。

- **水平分散**: 複数 GPU / 複数マシンでファイルを分割処理（コード変更不要）
  - RTX 3080 Ti + RTX 2070 SUPER 推定: AobaZero ~3,300 pos/sec (~25.6日), DL水匠 ~12,300 pos/sec (~6.9日)
- **TensorRT EP 再調査**: ORT ビルドと TRT バージョンの互換性を解決すれば大幅改善の可能性
- **FP16 許容**: DL水匠では実用的な精度（平均 5cp 差）で 1.4x 高速化
