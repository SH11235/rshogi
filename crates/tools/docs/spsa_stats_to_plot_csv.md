# SPSA 統計を plot CSV へ変換する

```bash
cargo run --release -p tools --bin spsa_stats_to_plot_csv -- stats.csv --output-csv stats.plot.csv --window 8
```

入力は seed 統計 CSV または旧 v3 の aggregate 統計 CSV です。`values.csv` や v4 batch 統計への対応を追加する変更ではありません。認識できない header はエラーになります。

`--output-csv` を省略すると入力名に `.plot.csv` を付けます。`--window` は 1 以上で、score_rate の移動平均の幅です（既定 8）。

入力と同じファイルへの出力は、同じパス・hardlink・symlink を含めて拒否します。出力 symlink は別ファイルを指す場合も拒否します。変換中は出力先と同じディレクトリの一時ファイルへ書き、全行の変換と flush が成功したときだけ出力を置き換えます。不正な途中行で失敗しても入力と既存の出力を保持し、一時ファイルを除去します。出力先ディレクトリは事前に用意してください。

出力には勝敗率、score_rate、累積と移動平均、更新量などの列が入ります。既存の集計式と列定義は変更していません。
