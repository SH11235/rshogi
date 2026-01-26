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

## NPS計測結果

### 最新ベンチマーク

計測条件: `--threads 1 --tt-mb 1024 --limit-type movetime --limit 20000`

#### NNUE評価時

| 局面 | 説明 | Depth | NPS | bestmove |
|:----:|------|:-----:|----:|----------|
| 1 | 序盤（9手目） | 14 | 642,550 | 1g1f |
| 2 | 中盤（詰将棋風） | 24 | 313,227 | 8d8f |
| 3 | 終盤（王手飛車） | 13 | 343,906 | N*6d |
| 4 | 終盤（詰み筋） | 22 | 325,481 | G*2h |
| **平均** | - | - | **406,291** | - |

#### Material評価時（NNUE無効、MaterialLevel=9）

| 局面 | 説明 | Depth | NPS | bestmove |
|:----:|------|:-----:|----:|----------|
| 1 | 序盤（9手目） | 16 | 315,116 | 2h2f |
| 2 | 中盤（詰将棋風） | 18 | 242,811 | 8d7d |
| 3 | 終盤（王手飛車） | 16 | 216,397 | N*4d |
| 4 | 終盤（詰み筋） | 17 | 221,531 | G*1c |
| **平均** | - | - | **248,964** | - |

### 最適化履歴

| 最適化 | NNUE NPS | Material NPS | 変化 |
|--------|--------:|-------------:|-----:|
| 前回（VNNI無効） | 359,149 | 244,334 | ベースライン |
| **今回（VNNI有効）** | **406,291** | 248,964 | **+13.1%** / +1.9% |

> **AVX512-VNNI DPBUSD命令最適化**により、Intel環境（Cascade Lake-X）でNNUE推論が**約13%高速化**。
> Material評価は誤差範囲内の変動。

---

## ホットスポット一覧

> **Phase 2で追加予定**: Intel VTune Profiler導入後にホットスポット分析を追加します。

---

## 計測方法

### 前提条件

- Windows環境
- Rust toolchain (`rustup` でインストール)
- NNUEファイル（`.bin`形式）

> **注意**: `--nnue-file` にはNNUEファイルのパスを指定してください。
> ファイルの配置場所は環境によって異なります（例: `./memo/nn.bin`）。

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

Linux/AMD Ryzen環境の計測結果（ベンチマーク結果とホットスポット分析）は [README.md](./README.md) を参照してください。

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
| 2025-12-22 | 初回計測実施: mainブランチでベースライン計測（NNUE: 359,149 NPS、Material: 244,334 NPS、movetime=20000ms）。各局面の詳細（Depth, Nodes, Time, Hashfull, Bestmove）を記録 |
| 2025-12-22 | 2回目計測: `nnue_vnni_dpbusd_support` ブランチで計測（NNUE: 406,291 NPS、Material: 248,964 NPS）。**AVX512-VNNI DPBUSD命令最適化によりNNUE推論が+13.1%高速化**を確認。Intel Cascade Lake-XのVNNI命令が効果を発揮 |
| 2025-12-22 | ドキュメント構造をREADME.md（Linux/AMD）と統一。NPS計測結果セクションのフォーマット変更、「最適化前/後」→「前回/今回」に文言変更 |
