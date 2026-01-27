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
- `engine_selfplay` - エンジン自己対戦
- `train_nnue` - NNUE学習（現在は [bullet-shogi](https://github.com/SH11235/bullet-shogi/tree/shogi-support) を使用）
- `generate_training_data` - 学習データ生成
- その他、データ前処理・変換ツール

## インストール

`Cargo.toml` に追加:

```toml
[dependencies]
rshogi-core = "0.1"
```

## ビルド

```bash
# ビルド
cargo build --release

# テスト実行
cargo test

# AVX2 SIMD最適化を有効にしてビルド
cargo build --release --features simd_avx2
```

## このエンジンを使用したアプリ

- [Ramu Shogi](https://ramu-shogi.sh11235.com/) - Web 将棋アプリ

## 参考・影響

本プロジェクトは将棋エンジン [YaneuraOu](https://github.com/yaneurao/YaneuraOu) およびチェスエンジン [Stockfish](https://github.com/official-stockfish/Stockfish) を参考にしています。

## ライセンス

GPL-3.0-only License - 詳細は [LICENSE](LICENSE) を参照してください。
