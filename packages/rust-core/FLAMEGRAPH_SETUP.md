# Flamegraph プロファイリングセットアップガイド

このドキュメントでは、shogi-core の性能分析のための flamegraph セットアップ手順を説明します。

## 前提条件

### Linux / WSL2
- `perf` コマンドが必要です
- Rust がインストールされていること

### macOS
- Xcode Command Line Tools がインストールされていること
- `dtrace` が利用可能（通常はデフォルトで入っています）

## セットアップ手順

### 1. flamegraph ツールのインストール

```bash
cargo install flamegraph
```

### 2. Linux/WSL2 での perf セットアップ

#### 標準的な Linux の場合
```bash
sudo apt-get update
sudo apt-get install -y linux-tools-common linux-tools-generic linux-tools-$(uname -r)
```

#### WSL2 の場合
WSL2 では専用のカーネルを使用しているため、標準の perf パッケージが直接使えません：

```bash
# generic tools をインストール
sudo apt-get install -y linux-tools-generic

# 利用可能な perf バイナリを確認
ls /usr/lib/linux-tools/*/perf

# シンボリックリンクを作成（例：6.8.0-63-generic の場合）
sudo ln -sf /usr/lib/linux-tools/6.8.0-63-generic/perf /usr/local/bin/perf
```

### 3. プロジェクトの設定

`Cargo.toml` に以下の設定が必要です（すでに設定済み）：

```toml
[profile.release]
debug = true  # デバッグシンボルを有効化
```

## プロファイリングの実行

### SEE (Static Exchange Evaluation) のプロファイリング

```bash
# フレームポインタを有効にして flamegraph を実行
RUSTFLAGS="-Cforce-frame-pointers=yes" cargo flamegraph --bin see_flamegraph -o see_profile.svg
```

### 一般的なベンチマークのプロファイリング

```bash
# ベンチマークを使用した場合
RUSTFLAGS="-Cforce-frame-pointers=yes" cargo flamegraph --bench see_bench -o see_bench_profile.svg
```

### カスタムバイナリの作成

より詳細な分析が必要な場合は、専用のプロファイリングバイナリを作成できます。
`src/bin/see_flamegraph.rs` のようなファイルを作成し、以下の要件を満たすようにします：

1. **実行時間**: 3-5秒程度
2. **サンプル数**: 十分なサンプリングデータを得るため、100万回以上のループ
3. **多様性**: 異なるパターンの処理を含める
4. **最適化回避**: `black_box()` を使用して最適化を防ぐ

## 結果の分析

生成された SVG ファイルをブラウザで開くと、以下の情報が確認できます：

1. **関数の幅** = CPU時間の消費割合
2. **スタックの深さ** = 呼び出し階層
3. **色** = 通常はランダム（見やすさのため）

### 注目すべきポイント

- **最も幅の広い関数**: 最適化の第一候補
- **予想外に太い関数**: 想定外のボトルネック
- **深いスタック**: 関数呼び出しのオーバーヘッド

## 補助的な分析ツール

### ハードウェアカウンタの利用

```bash
# キャッシュミスやCPUサイクルを測定
perf stat -e cache-misses,cache-references,cpu-cycles target/release/see_flamegraph
```

### 特定の関数を分離して分析

一時的に `#[inline(never)]` を追加して、インライン展開を防ぎ、
個別の関数として flamegraph に表示させることができます：

```rust
#[inline(never)]  // 一時的に追加
fn critical_function() {
    // ...
}
```

## トラブルシューティング

### "perf not found" エラー
- 上記の WSL2 セットアップ手順を確認
- `which perf` で perf の場所を確認

### シンボルが表示されない
- `[profile.release] debug = true` が設定されているか確認
- `RUSTFLAGS="-Cforce-frame-pointers=yes"` を忘れずに指定

### 結果が細かすぎる/粗すぎる
- 実行時間を調整（3-5秒が推奨）
- サンプリング頻度を調整: `--freq 99` など

## CI での自動化

GitHub Actions での例：

```yaml
- name: Install flamegraph
  run: cargo install flamegraph

- name: Run profiling
  run: |
    RUSTFLAGS="-Cforce-frame-pointers=yes" \
    cargo flamegraph --bin see_flamegraph -o flamegraph.svg

- name: Upload flamegraph
  uses: actions/upload-artifact@v3
  with:
    name: flamegraph
    path: flamegraph.svg
```

## 参考リンク

- [cargo-flamegraph](https://github.com/flamegraph-rs/flamegraph)
- [Brendan Gregg's Flame Graphs](https://www.brendangregg.com/flamegraphs.html)