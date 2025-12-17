# プロファイリングスクリプト

`perf`を使用したパフォーマンス分析用スクリプト集。

## 前提条件

- Linux環境
- `perf`コマンドがインストール済み（`sudo apt install linux-tools-generic`）
- sudo権限

## スクリプト一覧

| スクリプト | 用途 | ビルド |
|-----------|------|--------|
| `perf_profile.sh` | 基本的なホットスポット特定 | release + frame pointers |
| `perf_profile_debug.sh` | memset呼び出し元特定（シンボル解決） | debug |
| `perf_profile_nnue.sh` | NNUE有効時のプロファイリング | debug |
| `perf_reuse_search.sh` | SearchWorker再利用効果の測定 | release + debug symbols |

## 使用方法

```bash
cd packages/rust-core

# 基本的なホットスポット特定
./scripts/perf_profile.sh

# memset/memmoveの呼び出し元を特定（debug buildでシンボル解決）
./scripts/perf_profile_debug.sh

# NNUE有効時のプロファイリング
./scripts/perf_profile_nnue.sh

# SearchWorker再利用効果の測定
./scripts/perf_reuse_search.sh
```

## 出力

- 各スクリプトは`perf.data`を生成
- 詳細な解析は`sudo perf report`で対話的に確認可能

## 注意事項

- sudoが必要なため、CI/CDでの自動実行には向かない
- `perf.data`は`.gitignore`に追加済み
- 結果はシステム環境（CPU、OS）に依存する
