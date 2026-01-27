# プロファイリングスクリプト

`perf`を使用したパフォーマンス分析用スクリプト集。

## クイックスタート

全計測をまとめて実行し、ドキュメントを更新する場合:

```bash
# 1. 全計測を実行（sudo権限が必要）
./scripts/perf_all.sh

# 2. ドキュメントを更新（Claude Code）
# slash commandsの定義がrootにあるのでrepository rootでClaude Codeを起動していること
/update-rust-perf-docs
```

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
- 計測結果の分析ドキュメント: `docs/performance/README.md`
