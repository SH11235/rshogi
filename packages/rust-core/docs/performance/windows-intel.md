# パフォーマンス分析レポート (Windows/Intel)

このドキュメントは、将棋エンジン（Rust実装）のWindows/Intel環境でのパフォーマンス計測結果です。

## 計測環境

| 項目 | 値 |
|------|-----|
| CPU | Intel(R) Core(TM) i9-10900X CPU @ 3.70GHz |
| コア数 | 20 |
| OS | Windows |
| アーキテクチャ | x86_64 |
| 計測日 | 2025-12-22 |

---

## ベンチマーク結果 (NPS)

### NNUE有効時（本番相当）

**計測条件**: movetime=5000ms, threads=1, material_level=9

| 局面 | Depth | Nodes | Time (ms) | NPS | Hashfull | Bestmove |
|------|-------|-------|-----------|-----|----------|----------|
| 序盤 (9手目) | 14 | 2,543,616 | 4,769 | 533,364 | 4 | 1g1f |
| 中盤 (詰み有) | 21 | 1,369,088 | 4,750 | 288,229 | 2 | 8d8f |
| 終盤 (複雑) | 12 | 1,169,408 | 4,768 | 245,261 | 6 | N*4d |
| 終盤 (詰み有) | 16 | 1,438,720 | 4,778 | 301,113 | 8 | N*1g |
| **合計/平均** | - | 6,520,832 | 19,065 | **342,031** | - | - |

### Material評価時（NNUE無効）

**計測条件**: movetime=5000ms, threads=1, material_level=9

| 局面 | Depth | Nodes | Time (ms) | NPS | Hashfull | Bestmove |
|------|-------|-------|-----------|-----|----------|----------|
| 序盤 (9手目) | 14 | 1,553,408 | 4,776 | 325,252 | 2 | 5i5h |
| 中盤 (詰み有) | 16 | 1,204,224 | 4,777 | 252,087 | 7 | B*6h |
| 終盤 (複雑) | 14 | 1,104,896 | 4,789 | 230,715 | 7 | G*6b |
| 終盤 (詰み有) | 14 | 1,186,816 | 4,748 | 249,961 | 6 | G*3c |
| **合計/平均** | - | 5,049,344 | 19,090 | **264,502** | - | - |

---

## ホットスポット一覧

> **Phase 2で追加予定**: Intel VTune Profiler導入後にホットスポット分析を追加します。

---

## 計測方法

### 前提条件

- Windows環境
- Rust toolchain (`rustup` でインストール)
- NNUEファイル（`.bin`形式）

### ベンチマーク実行（NPS計測）

PowerShellまたはコマンドプロンプトで実行:

```powershell
cd packages/rust-core

# NNUE有効時
$env:RUSTFLAGS="-C target-cpu=native"
cargo run -p tools --bin benchmark --release -- `
  --internal --threads 1 --limit-type movetime --limit 20000 `
  --nnue-file ./path/to/nn.bin `
  --output-dir ./benchmark_results

# Material評価時（NNUE無効）
$env:RUSTFLAGS="-C target-cpu=native"
cargo run -p tools --bin benchmark --release -- `
  --internal --threads 1 --limit-type movetime --limit 20000 `
  --output-dir ./benchmark_results
```

Git Bashの場合:

```bash
cd packages/rust-core

# NNUE有効時
RUSTFLAGS="-C target-cpu=native" cargo run -p tools --bin benchmark --release -- \
  --internal --threads 1 --limit-type movetime --limit 20000 \
  --nnue-file ./path/to/nn.bin \
  --output-dir ./benchmark_results

# Material評価時（NNUE無効）
RUSTFLAGS="-C target-cpu=native" cargo run -p tools --bin benchmark --release -- \
  --internal --threads 1 --limit-type movetime --limit 20000 \
  --output-dir ./benchmark_results
```

### 結果ファイル

ベンチマーク結果はJSON形式で `benchmark_results/` ディレクトリに保存されます。

---

## Phase 2: ホットスポット分析（予定）

Intel VTune Profiler を使用してホットスポット分析を行う予定です。

### Intel VTune のインストール

1. [Intel oneAPI Base Toolkit](https://www.intel.com/content/www/us/en/developer/tools/oneapi/vtune-profiler.html) をダウンロード
2. VTune Profiler を選択してインストール

### VTune での計測（予定）

```powershell
# コマンドライン計測
vtune -collect hotspots -result-dir vtune_results -- ./target/release/engine-usi.exe

# レポート生成
vtune -report hotspots -result-dir vtune_results -format text -report-output hotspots.txt
```

---

## Linux/AMD環境との比較

Linux/AMD Ryzen環境の計測結果は [README.md](./README.md) を参照してください。

### アーキテクチャ間で注目すべき差異

| 項目 | 確認ポイント |
|------|-------------|
| AVX2 SIMD | Intel環境ではAVX2の効果が異なる可能性（AMD Zen 3では効果なし） |
| NPS絶対値 | クロック周波数やIPC特性により異なる |
| ホットスポット比率 | 同じコードでもCPU特性で相対比率が変わる可能性 |

---

## 変更履歴

| 日付 | 内容 |
|------|------|
| 2025-12-22 | ドキュメント作成（Phase 1: NPSベンチマークのみ） |
| 2025-12-22 | 初回計測実施（NNUE: 342,031 NPS、Material: 264,502 NPS）。各局面の詳細（Depth, Nodes, Time, Hashfull, Bestmove）を記録 |
