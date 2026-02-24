# パフォーマンス分析レポート (Windows/Intel)

このドキュメントは、将棋エンジン（Rust実装）のWindows/Intel環境でのパフォーマンス計測結果です。

## 計測環境

| 項目 | 値 |
|------|-----|
| CPU | Intel(R) Core(TM) i9-10900X CPU @ 3.70GHz |
| コア数 | 20 |
| OS | Windows |
| アーキテクチャ | x86_64 |
| 計測日 | 2026-02-24 |

---

## NPS計測結果

### 最新ベンチマーク

計測条件: `--threads 1 --tt-mb 1024 --limit-type movetime --limit 20000`

#### NNUE評価時

| 局面 | 説明 | Depth | NPS | bestmove |
|:----:|------|:-----:|----:|----------|
| 1 | 序盤（9手目） | 28 | 497,330 | 3g3f |
| 2 | 中盤（詰将棋風） | 17 | 442,376 | B*6h |
| 3 | 終盤（王手飛車） | 19 | 428,977 | N*4d |
| 4 | 終盤（詰み筋） | 19 | 368,583 | N*2c |
| **平均** | - | - | **434,307** | - |

#### Material評価時（NNUE無効、MaterialLevel=9）

| 局面 | 説明 | Depth | NPS | bestmove |
|:----:|------|:-----:|----:|----------|
| 1 | 序盤（9手目） | 23 | 277,976 | 5i6h |
| 2 | 中盤（詰将棋風） | 17 | 270,757 | 8d7d |
| 3 | 終盤（王手飛車） | 15 | 290,707 | N*4d |
| 4 | 終盤（詰み筋） | 17 | 283,655 | G*1c |
| **平均** | - | - | **280,773** | - |

### 最適化履歴

| 計測 | NNUE NPS | Material NPS | 変化 |
|------|--------:|-------------:|-----:|
| 前回（2025-12-22） | 406,291 | 248,964 | ベースライン |
| **今回（2026-02-24, suisho5.bin）** | **434,307** | **280,773** | **+6.9%** / +12.8% |

> **注意**: NNUEファイルや環境差分によりNPSは変動します。最適化効果の評価には同一条件での再計測が必要です。

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
# リポジトリルートで実行

# NNUE有効時
$env:RUSTFLAGS="-C target-cpu=native"
cargo run -p tools --bin benchmark --release -- `
  --internal --threads 1 --limit-type movetime --limit 20000 `
  --nnue-file ./eval/halfkp_256x2-32-32_crelu/suisho5.bin `
  --output-dir ./benchmark_results

# Material評価時（NNUE無効）
$env:RUSTFLAGS="-C target-cpu=native"
cargo run -p tools --bin benchmark --release -- `
  --internal --threads 1 --limit-type movetime --limit 20000 `
  --output-dir ./benchmark_results
```

Git Bashの場合:

```bash
# リポジトリルートで実行

# NNUE有効時
RUSTFLAGS="-C target-cpu=native" cargo run -p tools --bin benchmark --release -- \
  --internal --threads 1 --limit-type movetime --limit 20000 \
  --nnue-file ./eval/halfkp_256x2-32-32_crelu/suisho5.bin \
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
| 2025-12-22 | 2回目計測: `nnue_vnni_dpbusd_support` ブランチで計測（NNUE: 406,291 NPS、Material: 248,964 NPS） |
| 2025-12-22 | ドキュメント構造をREADME.md（Linux/AMD）と統一。NPS計測結果セクションのフォーマット変更、「最適化前/後」→「前回/今回」に文言変更 |
| 2026-02-24 | 計測更新: suisho5.bin 使用（NNUE: 434,307 NPS、Material: 280,773 NPS）。各局面のDepth/NPS/Bestmoveを更新 |
