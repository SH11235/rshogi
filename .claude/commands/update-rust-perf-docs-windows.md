# Rust エンジン パフォーマンスドキュメント更新 (Windows/Intel)

`packages/rust-core/docs/performance/windows-intel.md` を最新の計測結果で更新します。

## 手順

1. **ベンチマーク実行**

   以下の2つのベンチマークを実行する:

   - **所要時間**: 約3分（20秒 × 4局面 × 2モード = 160秒）
   - 初回はビルド時間が追加で必要

   ```bash
   cd packages/rust-core

   # NNUE有効時（20秒 × 4局面 = 約80秒）
   RUSTFLAGS="-C target-cpu=native" cargo run -p tools --bin benchmark --release -- \
     --internal --threads 1 --limit-type movetime --limit 20000 \
     --nnue-file ./memo/nn.bin \
     --output-dir ./benchmark_results

   # Material評価時（20秒 × 4局面 = 約80秒）
   RUSTFLAGS="-C target-cpu=native" cargo run -p tools --bin benchmark --release -- \
     --internal --threads 1 --limit-type movetime --limit 20000 \
     --output-dir ./benchmark_results
   ```

2. **計測結果の確認**
   - `packages/rust-core/benchmark_results/` ディレクトリに生成された最新の2つのJSONファイルを確認
   - NNUE有効時（`nnue_enabled: true`）と Material評価時（`nnue_enabled: false`）

3. **結果の読み取り**

   各JSONファイルから以下を抽出:

   **システム情報** (`system_info`):
   - `cpu_model`: CPU名
   - `cpu_cores`: コア数
   - `os`: OS名
   - `timestamp`: 計測日時

   **評価設定** (`eval_info`):
   - `nnue_enabled`: NNUE有効/無効
   - `nnue_file`: NNUEファイルパス（NNUE有効時のみ）
   - `material_level`: Material評価レベル

   **各局面の詳細** (`results[0].results[]`):
   - `sfen`: 局面（SFEN形式）
   - `depth`: 探索深さ
   - `nodes`: 探索ノード数
   - `time_ms`: 探索時間（ミリ秒）
   - `nps`: NPS（ノード/秒）
   - `hashfull`: 置換表使用率（‰）
   - `bestmove`: 最善手

4. **前回計測との比較分析**
   - `packages/rust-core/docs/performance/windows-intel.md` の現在の値と新しい計測値を比較
   - NPS変化率を計算（%）
   - 各局面のDepth変化にも注目（探索効率の指標）

5. **ドキュメント更新**
   - `packages/rust-core/docs/performance/windows-intel.md` の以下を更新:
     - 「計測環境」セクションのCPU情報・計測日
     - 「ベンチマーク結果 (NPS)」セクションのテーブル（各局面の詳細を含む）
     - 「変更履歴」に新しいエントリを追加

## 実行

最新の計測結果ファイルを読み込み、`packages/rust-core/docs/performance/windows-intel.md` を更新してください。

### テーブル形式

各セクション（NNUE有効時 / Material評価時）で以下の形式を使用:

```markdown
**計測条件**: movetime=20000ms, threads=1, material_level=9

| 局面 | Depth | Nodes | Time (ms) | NPS | Hashfull | Bestmove |
|------|-------|-------|-----------|-----|----------|----------|
| 序盤 (9手目) | 14 | 2,543,616 | 4,769 | 533,364 | 4 | 1g1f |
| 中盤 (詰み有) | 21 | 1,369,088 | 4,750 | 288,229 | 2 | 8d8f |
| 終盤 (複雑) | 12 | 1,169,408 | 4,768 | 245,261 | 6 | N*4d |
| 終盤 (詰み有) | 16 | 1,438,720 | 4,778 | 301,113 | 8 | N*1g |
| **合計/平均** | - | 6,520,832 | 19,065 | **342,031** | - | - |
```

### 局面の対応表

JSONの`sfen`フィールドと局面名の対応:

| SFEN（先頭部分） | 局面名 |
|-----------------|--------|
| `lnsgkgsnl/1r7/p1ppp1bpp/...` | 序盤 (9手目) |
| `l4S2l/4g1gs1/...` | 中盤 (詰み有) |
| `6n1l/2+S1k4/...` | 終盤 (複雑) |
| `l6nl/5+P1gk/...` | 終盤 (詰み有) |

### 注意事項

- Nodes, NPS値はカンマ区切りで記載（例: 2,543,616）
- 合計/平均行の計算:
  - Nodes: 全局面の合計
  - Time: 全局面の合計
  - NPS: 合計Nodes / 合計Time * 1000
- CPU情報はJSONの`system_info.cpu_model`から取得
- Linux/AMD環境との比較コメントがあれば追記

## Linux/AMD環境との比較について

Linux/AMD環境の結果（`README.md`）と比較して、以下の点に注目:

1. **NPS差異**: 同じコードでもCPU特性でNPSが異なる
2. **相対比率**: NNUE vs Material の相対差異
3. **今後の最適化候補**: Intel環境でのみ効果がありそうな最適化（例: AVX2 SIMD）
