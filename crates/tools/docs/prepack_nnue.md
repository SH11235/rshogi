# prepack_nnue

LayerStacks `.bin` の FT を事前展開し、FT / PSQT / Threat / FC の重みを64バイト整列した
読み取り専用配列として保存する。元の `.bin` は変更しない。

```bash
cargo run --release -p tools --no-default-features --features prepacked-nnue --bin prepack_nnue -- \
  --input "$SHOGI_DATA/nnue/model.bin" --output "$SHOGI_DATA/nnue/model.packed"
```

`--input` と `--output` は必須。出力先が存在すると失敗する。
変換中に入力を変更しないこと。変換失敗時には未完成ファイルが残る場合がある。
再試行には別の出力先を指定するか、未完成の出力を確認して削除する。
FT は固定長バッファで展開する。後段の FC は変換時に次の配置へ並べる。

- `--fc-layout row-major`（既定）: universal edition 用。
- `--fc-layout native`: 変換プログラムと同じ ISA 設定でビルドした固定 edition 用。

各層に形状と配置 ID を保存し、読み込み先と合わなければエラーにする。
固定 edition の ISA 設定が異なる場合は、そのビルド設定で変換プログラムを再ビルドして
変換する。元ファイルの SHA-256 とコンテナのチェックサムを格納する。
係数調整時は編集する FC tensor だけを私有配列へコピーし、共有ファイルを変更しない。

読み込むエンジンにも `prepacked-nnue` feature が必要。
edition に合わせた feature と併用してビルドし、USI `EvalFile` に出力ファイルを指定する。
通常の `.bin` の読み込みも継続して使える。モデルの次元・FT・PSQT・Threat profile の
互換性は変換後も読み込み先の edition に依存する。

Windows ではファイルを読み取り専用で map し、使用中の書き込み・削除を拒否する。
複数プロセスで同一ファイルを指定すると重みを共有できる。
その他の OS は整列した所有配列へ読み込む。非 Windows で読み込み中のファイルを変更しないこと。
展開済みファイルは元の圧縮 `.bin` より大きくなる。起動時間と探索速度への効果は
モデル・ストレージ・プロセス数に依存するため、利用条件で比較する。
