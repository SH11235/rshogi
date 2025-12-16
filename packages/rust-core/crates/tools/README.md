## 将棋エンジンベンチマークツール

YaneuraOu の `bench` コマンド相当の標準ベンチマーク機能を提供します。

### 機能

- **内部APIモード**: Rust の探索 API を直接呼び出してベンチマーク
- **USIモード**: 外部エンジンバイナリを USI プロトコル経由で測定
- **複数スレッド対応**: スレッド数別のスケーリング測定
- **並列効率計算**: 理想的なスケーリングとの比較

### クイックスタート

#### 内部APIモード（自作エンジン）

```bash
cargo run -p tools --bin benchmark --release -- --internal
```

#### USIモード（外部エンジン）

```bash
cargo run -p tools --bin benchmark --release -- \
  --engine /path/to/engine \
  --threads 1,2,4,8
```

### コマンドラインオプション

| オプション | 説明 | デフォルト |
|-----------|------|-----------|
| `--threads` | 測定するスレッド数（カンマ区切り） | 1 |
| `--tt-mb` | 置換表サイズ（MB） | 1024 |
| `--limit-type` | 制限タイプ (depth/nodes/movetime) | movetime |
| `--limit` | 制限値 | 15000 |
| `--sfens` | カスタム局面ファイル | デフォルト4局面 |
| `--iterations` | 反復回数 | 1 |
| `--output-dir` | 結果JSON出力ディレクトリ | ./benchmark_results |
| `-v, --verbose` | 詳細なinfo行を表示 | false |
| `--engine` | エンジンバイナリパス | なし（内部API） |
| `--internal` | 内部API直接呼び出しモード | false |

### カスタム局面ファイル

以下の形式で SFEN 局面を指定できます：

```
# コメント行
局面名1 | sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1
局面名2 | sfen ...

# 区切り文字がない場合は、SFEN文字列のみ
lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1
```

### 出力形式

#### コンソール出力

```
=== Benchmark Summary ===
Engine: YaneuraOu
CPU: AMD Ryzen 9 5950X 16-Core Processor
Cores: 32
OS: Ubuntu

Threads    Total Nodes     Total Time      Avg NPS         Efficiency
----------------------------------------------------------------------
1          30,331,856      19,997ms        1,516,817       100.0%
2          60,569,719      19,999ms        3,028,594       99.8%
4          120,560,234     19,998ms        6,028,853       99.4%
8          241,476,716     19,998ms        12,075,335      99.6%
```

#### JSON出力

結果は `benchmark_results/` に自動保存されます：
- ファイル名形式: `YYYYMMDDhhmmss_enginename_threads.json`
- システム情報、エンジン情報、全測定結果を含む

### ライブラリとしての使用

```rust
use tools::{BenchmarkConfig, LimitType, runner};

let config = BenchmarkConfig {
    threads: vec![1, 2, 4],
    tt_mb: 1024,
    limit_type: LimitType::Depth,
    limit: 10,
    sfens: None,
    iterations: 1,
    verbose: false,
};

let report = runner::internal::run_internal_benchmark(&config)?;
report.print_summary();
report.save_json(&output_path)?;
```

### トラブルシューティング

#### エンジンがハングする

- `--limit` の値を小さくする
- `--verbose` でinfo行を確認

#### 測定結果が不安定

- `--iterations` を増やして平均を取る
- システムの他のプロセスを停止
- CPU の省電力機能を無効化
