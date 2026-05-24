# rshogi

Rust で書かれた将棋エンジンです。

[![Crates.io](https://img.shields.io/crates/v/rshogi-core.svg)](https://crates.io/crates/rshogi-core)
[![Documentation](https://docs.rs/rshogi-core/badge.svg)](https://docs.rs/rshogi-core)
[![License: GPL-3.0](https://img.shields.io/badge/License-GPLv3-blue.svg)](LICENSE)

## 特徴

- **ビットボードによる盤面表現** - 高速な合法手生成と局面評価
- **NNUE評価関数** - HalfKP / HalfKA / HalfKA_hm アーキテクチャ対応
- **Alpha-Beta探索** - 各種枝刈り技法（Null Move, Futility, LMR など）
- **置換表** - ロックフリーな並行ハッシュテーブル
- **時間管理** - 適応的な時間制御
- **並列探索** - Lazy SMP によるマルチスレッド探索
- **USIプロトコル** - Universal Shogi Interface 対応

## オプショナルな独自ルール

- **パス権利** - 1手パスできる機能。ハンデ戦や遊び要素として使用可能

## クレート構成

```
crates/
├── rshogi-core/    # エンジンコアライブラリ
├── rshogi-usi/     # USI実行バイナリ
└── tools/          # 開発・学習用ツール群
```

### rshogi-core

[![Crates.io](https://img.shields.io/crates/v/rshogi-core.svg)](https://crates.io/crates/rshogi-core)

エンジンのコアライブラリ。盤面表現、合法手生成、探索、NNUE評価などを提供。

### rshogi-usi

USI（Universal Shogi Interface）プロトコル実装。将棋GUIから呼び出せる実行バイナリ。

### tools

開発・学習用のツール群:

- `benchmark` - 探索性能ベンチマーク
- `bench_nnue_eval` - NNUE推論性能ベンチマーク
- `gensfen` - NNUE 学習用 PSV/pack 教師局面生成
- `tournament` - エンジン間棋力比較・SPRT
- 教師データ処理（shuffle, rescore, preprocess 等）
- SPSA パラメータチューニング

NNUE 学習には [bullet-shogi](https://github.com/SH11235/bullet-shogi/tree/shogi-support) を使用しています。

## インストール

`Cargo.toml` に追加:

```toml
[dependencies]
rshogi-core = "0.2"
```

## ビルド

`.cargo/config.toml` で `target-cpu=native` を採用しているため、`cargo build` だけで
build マシンの SIMD (AVX2 等) は自動で有効になる。

```bash
# 開発用 build (全 NNUE arch を runtime dispatch)
cargo build --release

# テスト実行
cargo test
```

複数 NNUE architecture binary を `engines/` 配下で並行管理したい場合 (selfplay / SPRT /
tournament 等) は `cargo xtask` 経由で preset edition ごとに別 binary を build できる:

```bash
# 利用可能な preset edition を列挙
cargo xtask list-editions

# 特定 preset を build (engines/rshogi-usi-<edition> に配置 + .meta.toml 記録)
cargo xtask build --edition ls-halfka_hm_merged-1536x16x32-psqt

# 複数 preset を一気に build
cargo xtask build --edition X,Y
cargo xtask build --all-presets

# engines/ 配下の binary 一覧 (preset / commit / age / status を表示)
cargo xtask list-binaries
```

詳細は [`docs/build.md`](docs/build.md) と ADR
[`docs/decisions/2026-05-24-build-edition-flavor-design.md`](docs/decisions/2026-05-24-build-edition-flavor-design.md)
を参照。

## このエンジンを使用したアプリ

- [Ramu Shogi](https://ramu-shogi.sh11235.com/) - Web 将棋アプリ

## 参考・影響

本プロジェクトは将棋エンジン [YaneuraOu](https://github.com/yaneurao/YaneuraOu) およびチェスエンジン [Stockfish](https://github.com/official-stockfish/Stockfish) を参考にしています。

## ライセンス

GPL-3.0-only License - 詳細は [LICENSE](LICENSE) を参照してください。
