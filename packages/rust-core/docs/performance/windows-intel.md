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

### 最適化効果サマリ

| 評価 | main (ベースライン) | nnue_vnni_dpbusd_support | 変化 |
|------|---------------------|--------------------------|------|
| **NNUE** | 359,149 | 406,325 | **+13.1%** |
| **Material** | 244,334 | 248,980 | +1.9% (誤差範囲) |

> **AVX512-VNNI DPBUSD命令最適化**により、Intel環境（Cascade Lake-X）でNNUE推論が**約13%高速化**。

---

### NNUE有効時（本番相当）

**計測条件**: movetime=20000ms, threads=1, material_level=9

#### nnue_vnni_dpbusd_support ブランチ（最適化あり）

| 局面 | Depth | Nodes | Time (ms) | NPS | Hashfull | Bestmove |
|------|-------|-------|-----------|-----|----------|----------|
| 序盤 (9手目) | 14 | 12,705,792 | 19,774 | 642,550 | 16 | 1g1f |
| 中盤 (詰み有) | 24 | 6,189,056 | 19,759 | 313,227 | 25 | 8d8f |
| 終盤 (複雑) | 13 | 6,796,288 | 19,762 | 343,906 | 31 | N*6d |
| 終盤 (詰み有) | 22 | 6,433,792 | 19,767 | 325,481 | 24 | G*2h |
| **合計/平均** | - | 32,124,928 | 79,062 | **406,325** | - | - |

#### main ブランチ（ベースライン）

| 局面 | Depth | Nodes | Time (ms) | NPS | Hashfull | Bestmove |
|------|-------|-------|-----------|-----|----------|----------|
| 序盤 (9手目) | 14 | 11,219,968 | 19,776 | 567,379 | 14 | 1g1f |
| 中盤 (詰み有) | 23 | 5,541,888 | 19,762 | 280,436 | 22 | 8d8f |
| 終盤 (複雑) | 13 | 5,812,224 | 19,768 | 294,020 | 27 | N*6d |
| 終盤 (詰み有) | 21 | 5,829,632 | 19,780 | 294,724 | 22 | G*2h |
| **合計/平均** | - | 28,403,712 | 79,086 | **359,149** | - | - |

### Material評価時（NNUE無効）

**計測条件**: movetime=20000ms, threads=1, material_level=9

#### nnue_vnni_dpbusd_support ブランチ（最適化あり）

| 局面 | Depth | Nodes | Time (ms) | NPS | Hashfull | Bestmove |
|------|-------|-------|-----------|-----|----------|----------|
| 序盤 (9手目) | 16 | 6,232,064 | 19,777 | 315,116 | 15 | 2h2f |
| 中盤 (詰み有) | 18 | 4,793,344 | 19,741 | 242,811 | 32 | 8d7d |
| 終盤 (複雑) | 16 | 4,276,224 | 19,761 | 216,397 | 26 | N*4d |
| 終盤 (詰み有) | 17 | 4,376,576 | 19,756 | 221,531 | 25 | G*1c |
| **合計/平均** | - | 19,678,208 | 79,035 | **248,980** | - | - |

#### main ブランチ（ベースライン）

| 局面 | Depth | Nodes | Time (ms) | NPS | Hashfull | Bestmove |
|------|-------|-------|-----------|-----|----------|----------|
| 序盤 (9手目) | 16 | 6,098,944 | 19,780 | 308,339 | 15 | 2h2f |
| 中盤 (詰み有) | 18 | 4,704,257 | 19,763 | 238,034 | 31 | 8d7d |
| 終盤 (複雑) | 16 | 4,189,184 | 19,767 | 211,923 | 25 | N*4d |
| 終盤 (詰み有) | 17 | 4,332,544 | 19,782 | 219,019 | 25 | G*1c |
| **合計/平均** | - | 19,324,929 | 79,092 | **244,334** | - | - |

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
| 2025-12-22 | 初回計測実施（NNUE: 342,031 NPS、Material: 264,502 NPS、movetime=5000ms）。各局面の詳細（Depth, Nodes, Time, Hashfull, Bestmove）を記録 |
| 2025-12-22 | mainブランチでベースライン計測実施（NNUE: 359,149 NPS、Material: 244,334 NPS）。`nnue_vnni_dpbusd_support` ブランチとの比較で**AVX512-VNNI DPBUSD最適化によりNNUE推論が+13.1%高速化**を確認 |
