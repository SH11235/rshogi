# apply_net_spsa_params

SPSA が出力した net 係数 delta を元の LayerStacks NNUE `.bin` に焼き込み、delta 指定なしで
同じ係数を持つ新しい `.bin` を作る。

## 基本形

```bash
expected_net_sha256="$(sed -n 's/^# .*sha256=\([[:xdigit:]]\{64\}\).*/\1/p' /path/to/net.params)"
test "${#expected_net_sha256}" -eq 64
cargo run --release -p tools --bin apply_net_spsa_params -- \
  --nnue "$SHOGI_DATA/nnue/base.bin" \
  --params runs/spsa/net-example/final.params \
  --expected-net-sha256 "$expected_net_sha256" \
  --output "$SHOGI_DATA/nnue/tuned.bin" \
  --report runs/spsa/net-example/finalize.json
```

`.params` の value 列は浮動小数を `round()` して整数 delta にする。`[[NOT USED]]` 行は
無視する。通常の探索定数を含む `SPSA_NET_` 以外の有効行や、`int` 以外の行は入力の
取り違えとしてエラーにする。

## 入力 net の照合

`generate_net_spsa_params` がコメントへ記録した SHA-256 が `.params` に残っていれば、
`--nnue` の SHA-256 と照合する。`spsa` の `state.params` / `final.params` はコメント行を
保存しないため、推奨運用は SPSA 開始時の generator 出力から SHA-256 を控え、焼き込み時に
`--expected-net-sha256 <hex>` で渡すこと。metadata と option が両方ある場合は一致必須である。

照合元がなく、`--expected-net-sha256` も省略した場合はエラーになる。意図的に出所を照合せず
適用する場合、または照合した hash と異なる net へ適用する場合だけ
`--allow-net-mismatch` を指定する。不正な SHA-256 や metadata と option の食い違いは
この option でも許可しない。

## 書換えと検証

- output / L2 の `i8` weight と output の `i32` bias は対象 byte だけを書き換える。
- Feature Transformer bias は signed LEB128 を decode し、engine と同じ saturating 加算後に
  最短形で再 encode する。
- Split / Combined とも bias 部分だけを decode・再 encode する。Combined の size field は
  新しい bias 長と元の weight payload 長から更新する。weight payload は decode せず、元の
  byte 列をそのまま転送する。再 encode 対象の bias は `i16` 範囲と最短 LEB128 を検証する。
- header、architecture 文字列、FT weight、PSQT、Threat、対象外 bucket／tensor は入力から
  そのままコピーする。delta が空または全て 0 ならファイル全体が byte 一致する。

入力は一度だけ `File` として open し、同じ descriptor を rewind しながら SHA-256、layout、
delta 適用に使う。最終パスと同じディレクトリの一時ファイルへ、固定 1 MiB buffer で
ストリーミング出力する。常駐するのは入力 layout が保持する FT bias と全 bucket の小さい
FC block、再 encode 後の FT bias、転送 buffer だけで、FT weights や net 全体のサイズには
依存しない。入出力 SHA-256 もファイルからストリーミング計算する。

書き込み後は一時ファイルを開き直し、feature 非依存の `.bin` layout reader で末尾余りなく
解析する。入力 layout から先に作った対象係数だけの期待値 map と、出力 layout が保持する
値を比較し、engine と同じ saturating delta の適用を検証する。検証後に net を rename で
確定し、output と report の実体が異なることを再確認してから report を確定する (net の無い
report を残さない)。この事後確認で alias を検出した場合は report を書かずエラーにし、正しく
確定済みの net は残す。検証・書き込み失敗時に部分ファイルを最終パスへ残さない。入力と出力に
同じファイルは指定できない。

入力と同じ version、architecture、LEB128 ブロック構成を維持し、末尾余りのない形式を
layout reader と core 側の engine 等価性テストで確認している。このため tatara の
`net_to_yo` に渡す形式は変わらない。YO 変換の実機確認は tatara 側で行う。

## JSON report

`--report <path>` を指定すると次を JSON へ出力する。

```json
{
  "applied_count": 128,
  "clamped_count": 2,
  "nonzero_delta_count": 117,
  "input_sha256": "...",
  "output_sha256": "...",
  "source": {
    "nnue": "/path/to/base.bin",
    "params": "runs/spsa/net-example/final.params"
  }
}
```

`applied_count` は `[[NOT USED]]` を除く有効行数、`nonzero_delta_count` はそのうち丸め後の
delta が 0 でない行数、`clamped_count` は格納型の範囲へ saturate した適用回数である。

## 台帳

実験台帳には少なくとも次を残す。

- 入力 `.bin` のパスと SHA-256
- SPSA の `final.params` のパスと run / seed
- 出力 `.bin` のパスと SHA-256
- 適用数、非ゼロ delta 数、clamp 数
- 独立 seed の SPRT 結果

### 重複パラメータの拒否

同じ名前の行、および同じ net 係数 ID を表す行（数値 index の先頭ゼロなど）は、元ファイルの行番号を示して拒否します。`[[NOT USED]]` 行も重複検査の対象です。重複行を後勝ちまたは加算と解釈しません。既存ファイルは自動修正しないため、意図した 1 行へ整理してください。
