# プロファイリングスクリプト

`perf`を使用したパフォーマンス分析用スクリプト集。

## クイックスタート

全計測をまとめて実行する場合:

```bash
# 全計測を実行（sudo権限が必要）
./scripts/perf_all.sh
```

結果は `perf_results/` と `benchmark_results/` に保存されます。

## 前提条件

- Linux環境
- `perf`コマンドがインストール済み（`sudo apt install linux-tools-generic`）
- sudo権限

## 設定ファイル

NNUEファイルのパスなどの個人設定は `scripts/perf.conf` で管理します。

```bash
# 初回セットアップ
cp scripts/perf.conf.example scripts/perf.conf

# 環境に合わせて編集
vim scripts/perf.conf
```

`perf.conf` は `.gitignore` に含まれているため、個人の環境設定をバージョン管理に含めずに済みます。

設定ファイルがない場合は自動的に `perf.conf.example` からコピーされ、編集を促すエラーで終了します。

## スクリプト一覧

### 統合スクリプト

| スクリプト | 用途 |
|-----------|------|
| `perf_all.sh` | **全計測をまとめて実行**（perf + benchmark、推奨） |

### 個別スクリプト

| スクリプト | 用途 | ビルド | perf data | 出力ファイル |
|-----------|------|--------|-----------|--------------|
| `perf_profile.sh` | 基本的なホットスポット特定 | release + frame pointers | `perf_release.data` | `YYYYMMDD_HHMMSS_release.txt` |
| `perf_profile_debug.sh` | memset呼び出し元特定（シンボル解決） | debug | `perf_debug.data` | `YYYYMMDD_HHMMSS_debug.txt` |
| `perf_profile_nnue.sh` | NNUE有効時のプロファイリング | release/debug | `perf_nnue.data` | `YYYYMMDD_HHMMSS_nnue_<mode>.txt` |
| `perf_reuse_search.sh` | SearchWorker再利用効果の測定 | release + debug symbols | `perf_reuse.data` | `YYYYMMDD_HHMMSS_reuse_search.txt` |

## 使用方法

```bash
# 基本的なホットスポット特定
./scripts/perf_profile.sh

# memset/memmoveの呼び出し元を特定（debug buildでシンボル解決）
./scripts/perf_profile_debug.sh

# NNUE有効時のプロファイリング（推奨）
./scripts/perf_profile_nnue.sh
./scripts/perf_profile_nnue.sh --movetime 10000  # movetimeを10秒に設定
./scripts/perf_profile_nnue.sh --debug           # debug build
./scripts/perf_profile_nnue.sh --nnue-file /path/to/nn.bin  # NNUEファイル指定

# 全計測（NNUEファイル指定）
./scripts/perf_all.sh --nnue-file /path/to/nn.bin

# SearchWorker再利用効果の測定
./scripts/perf_reuse_search.sh
```

## 出力

### 自動保存

各スクリプトは結果を `./perf_results/` ディレクトリにタイムスタンプ付きで自動保存します。

```
perf_results/
├── 20251218_120000_nnue_release.txt
├── 20251218_130000_release.txt
├── 20251218_140000_debug.txt
└── ...
```

### 対話的な分析

```bash
# 各スクリプトのperf dataから詳細な解析
sudo perf report -i perf_nnue.data
sudo perf report -i perf_release.data
sudo perf report -i perf_debug.data
sudo perf report -i perf_reuse.data
```

## 注意事項

- sudoが必要なため、CI/CDでの自動実行には向かない
- `perf.data`と`perf_results/`は`.gitignore`に追加済み
- 結果はシステム環境（CPU、OS）に依存する
- 複数スクリプトの並列実行が可能（各スクリプトは別々の `perf_*.data` ファイルを使用）

## wasm の計測

`run_wasi.mjs` は、WASI 向けにビルドした wasm を Node.js 20 以降で動かす (wasm の速さを V8 で測る。wasm から見えるのはカレントディレクトリだけ)。
`node scripts/run_wasi.mjs <file.wasm> [args...]`。詰み探索の局面集ベンチでの使い方は
`crates/tools/docs/mate_bench.md` の「wasm で測る」。

## fixed edition の WASI 起動検証

`node scripts/check_fixed_wasm.mjs` は 5 FT の `1536x16x32-none` edition を、
SIMD128 無効・有効の順に direct `cargo build` し、各 build の直後に Node WASI で起動を検証します。
全処理は直列で、Cargo は jobs=2 です。Node.js 20 以降と、リポジトリで指定した Rust toolchain の
`wasm32-wasip1` target が必要です。target は事前に `rustup target add wasm32-wasip1` で追加してください。

```sh
node scripts/check_fixed_wasm.mjs
node scripts/check_fixed_wasm.mjs --ft halfka_hm_merged
```

`--ft` は `halfkp`、`halfka_split`、`halfka_merged`、`halfka_hm_split`、`halfka_hm_merged` の
いずれか一つを選びます。省略時は全部を検証します。build の上限は各 30 分、起動は各 60 秒です。
失敗・timeout・`usiok` / `readyok` の不足は非ゼロ終了となります。

全構成で一つの target directory を使います。既定は repo の `target/fixed-edition-wasm`。
`CARGO_TARGET_DIR` があればそれを使い、相対指定はスクリプトを起動したディレクトリから解決します。
同じ target directory への別 build と並行実行しないでください。host 用 RUSTFLAGS と encoded flags は
子プロセスで除去し、`+simd128` / `-simd128` を明示します。

smoke は `MaterialLevel=1` の USI 初期化だけです。NNUE ファイルのロード・forward 数値・探索・browser
埋込みを検証するものではありません。初期化時には既存の USI 仕様により TT を 256 MiB 確保します。

通常 CI は代表として `halfka_hm_merged` の SIMD128 有無を確認します。全 5 FT の確認は引数なしで実行してください。
