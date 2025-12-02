# 将棋ゲームエンジン（Shogi Game Engine）

Rust実装の将棋エンジンプロジェクトです。NNUE（Efficiently Updatable Neural Network）評価関数を搭載し、USIプロトコルに対応しています。

## 📦 パッケージ構成

```
packages/
└── rust-core/              # 将棋AIエンジン（Rustワークスペース）
    ├── crates/
    │   ├── engine-core/    # コアエンジン実装（152ファイル）
    │   ├── engine-usi/     # USIプロトコルCLIインターフェース
    │   └── tools/          # NNUE訓練・解析ツール（60以上のバイナリ）
    ├── docs/               # 包括的なドキュメント（50以上のマークダウンファイル）
    └── Cargo.toml          # ワークスペース定義

apps/                       # 今後追加予定：GUIアプリケーション等
```

## 📄 ライセンス

MIT License
