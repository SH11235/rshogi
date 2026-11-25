# eval_sfens

LayerStacks ネットは最大 64 bucket（PSQT 付きは最大 32）に対応します。`ls_bucket=` は最大 3 軸です。

SFEN テキスト（1 行 1 局面）を LayerStacks NNUE で静的評価し、TSV を標準出力へ出す。

progress 係数の保持・再読込みの契約は [routing 対応表](nnue-routing.md) を参照してください。

### ヘッダに `ls_bucket=` があるネット

`ls_bucket=ownking:2/3/7+oppking:2/3/7` のような spec をモデルが持つ場合は、
エンジンと同じ routing で bucket を自動選択する。

```sh
cargo run --release -p tools --bin eval_sfens -- \
  --nnue "$SHOGI_DATA/nnue/<spec-model>.bin" \
  --sfens sfens.txt
```

`--bucket-mode` は省略できる。`progresskpabs` を明示した場合もヘッダの spec を使う。
`--bucket-mode progresskpabsq16`、`--bucket-mode kingrank9`、`--progress-buckets`、`--progress-coeff` は競合エラーになる。

### ヘッダに spec がないネット

進行度方式では `--bucket-mode progresskpabs`（既定）か `--bucket-mode progresskpabsq16` を選び、
`--progress-buckets` を指定する。玉段の 9 bucket モデルには `--bucket-mode kingrank9` を指定し、
進行度の引数は省略する。

```sh
cargo run --release -p tools --bin eval_sfens -- \
  --nnue "$SHOGI_DATA/nnue/<model>.bin" \
  --sfens sfens.txt \
  --progress-coeff "$SHOGI_DATA/progress/<coeff>.bin" \
  --progress-buckets 8 \
  --count 10
```

### 出力と診断

`--count` の既定値は 10 で、入力の先頭から読む行数を制限する（空行も数えるが評価はしない）。
出力列は `sfen`、`bucket`、`raw`、`score`（歩=90 の内部スケール）、
`score_cp`（歩=100 の cp）。

`--dump-debug-first` で最初の評価局面の中間値を標準エラーへ出す。
L1 の診断は読み取り専用の重みを使い、従来の密 SIMD 計算とスカラー計算を比較する。

AVX-512F/BW ビルドでは、駒 FT 幅が 32 の倍数で、駒 FT の重みがすべて −254 から 254 の
範囲の偶数なら、駒 FT のメモリ使用量を削減する。評価値と診断値は i16 格納時と同じ。
Linux で共有メモリの新規確保に失敗した重みは、プロセス内のメモリで保持する。

### L2=64 の専用モデル

静的 LayerStacks の解析には、モデルのサイズ (shortcut を除いた数え方) に合わせて
`--features layerstacks-1024x15x64`、`--features layerstacks-1536x15x64`、
`--features layerstacks-2304x15x64` または `--features layerstacks-3072x15x64`
をビルド時に追加する。L2 の中間出力も指定した幅で計算する。

HalfKA_hm merged 1536x15x64 (ヘッダーの実寸法は 1536x16x64)、Threat 幅 512 のモデル向けのビルド例。

```sh
cargo build --release -p tools --bin eval_sfens --no-default-features \
  --features layerstack-arch,layerstacks-1536x15x64,ft-halfka_hm_merged,nnue-threat,rshogi-core/threat-width-512,rshogi-core/mode-specific
```

LayerStacks（SplitSkip）の Threat 付きモデルでは、`score` (と `score_cp`) はエンジンと同じく Threat を含むが、TSV の `raw` 列と
`--dump-debug-first` の中間値は駒特徴だけから計算する。tatara の `eval_sfen` と照合するときは `bucket` と `score` を比べ、
`raw` は Threat・PSQT なしのモデルでだけ比べる。

起動時の bucket 数・係数の組合せと入力エラー時の扱いは、[native routing の設定条件](nnue-routing.md#起動時の設定条件)を参照してください。

### LayerStacks-DenseConcat

FT 幅と Threat 幅に合う [DenseConcat 専用 edition](../../../docs/build.md#layerstacks-denseconcat) を指定する。
以下は 1536-16-64 の Threat なしの例。2304-16-64 では feature 名の `1536x16x64` を `2304x16x64` に置き換える。

```sh
cargo run --release -p tools --bin eval_sfens --no-default-features \
  --features edition-layerstacks-dense-concat-halfka_hm_merged-1536x16x64-none -- \
  --nnue "$SHOGI_DATA/nnue/<dense-concat>.bin" --sfens sfens.txt --count 10
```

上の例はヘッダーに `ls_bucket=` があるモデル用。ヘッダーに指定がなければ学習時の routing 引数を加える。
Threat 付きなら feature の末尾を `threat`、`threat256`、`threat512`、`threat768` のいずれかに合わせる。`threat256` は 1536-16-64 専用。
DenseConcat の `raw` は Threat を含む `fc_2 + skip` で、PSQT の加算とスケーリングの前の整数値。
tatara の `--debug` が出す `bucket` と `raw`、内部スケールの `score` を照合する。
`--dump-debug-first` の駒 FT と第 1 affine 層の診断値は駒特徴だけから計算する。

### tatara との手動照合

以下は HalfKA_hm merged 1536x15x64、Threat・PSQT なしの spec 付きネットを照合する例。
rshogi のルートで実行する。`NETS` に比較する `.bin` のパスを並べ、`EXTRA_SFENS` に
平手初期局面と玉を進めた局面から合法手をランダムに指して採取した SFEN ファイルを指定する。
4 件の sentinel だけで試す場合は `EXTRA_SFENS=/dev/null` を指定する。

```bash
set -e
export CARGO_BUILD_JOBS=8
cargo build --release -p tools --bin eval_sfens --no-default-features \
  --features rshogi-core/edition-layerstacks-halfka_hm_merged-1536x15x64-none,layerstack-arch,layerstacks-1536x15x64,ft-halfka_hm_merged
(cd /path/to/tatara && cargo build --release -p eval-sfen --target-dir /tmp/tatara-evalsfen-target)
export TATARA_EVAL=/tmp/tatara-evalsfen-target/release/eval_sfen
export RSHOGI_EVAL=target/release/eval_sfens
export EXTRA_SFENS=/path/to/generated.sfens
export FV_SCALE=14
NETS=("$SHOGI_DATA/nnue/<first-model>.bin" "$SHOGI_DATA/nnue/<second-model>.bin")

python3 - "${NETS[@]}" <<'PY'
import csv
import io
import os
from pathlib import Path
import subprocess
import sys
import tempfile

sentinel = Path("crates/tools/docs/fixtures/search_only_sentinel_4pos.txt")
positions = [line.split(" | ", 1)[1] for line in sentinel.read_text().splitlines()
             if " | " in line and not line.startswith("#")]
positions += [line.strip() for line in Path(os.environ["EXTRA_SFENS"]).read_text().splitlines()
              if line.strip()]
scale = os.environ["FV_SCALE"]

with tempfile.TemporaryDirectory() as temp:
    sfens = Path(temp) / "positions.sfens"
    sfens.write_text("\n".join(positions) + "\n")
    for net in sys.argv[1:]:
        output = subprocess.check_output(
            [os.environ["RSHOGI_EVAL"], "--nnue", net, "--sfens", sfens,
             "--count", str(len(positions))], text=True)
        rows = list(csv.DictReader(io.StringIO(output), delimiter="\t"))
        reference = subprocess.run(
            [os.environ["TATARA_EVAL"], "--nnue-file", net, "--fv-scale", scale, "--debug"]
            + [arg for sfen in positions for arg in ("--sfen", sfen)],
            check=True, text=True, capture_output=True)
        traced = [line.split("\t") for line in reference.stderr.splitlines()
                  if "\tbucket=" in line]
        scores = reference.stdout.splitlines()
        if len(rows) != len(traced) or len(rows) != len(scores) or len(rows) != len(positions):
            raise SystemExit(f"{net}: unexpected number of evaluation rows")
        for sfen, row, trace, score in zip(positions, rows, traced, scores):
            expected = (sfen, trace[1].split("=")[1], trace[2].split("=")[1], score)
            actual = (row["sfen"], row["bucket"], row["raw"], row["score"])
            if trace[0] != sfen or actual != expected:
                raise SystemExit(f"{net}: {sfen}: expected {expected}, got {actual}")
        buckets = sorted({int(row["bucket"]) for row in rows})
        print(f"{net}: {len(rows)}/{len(positions)} match, buckets={buckets}")
PY
```

tatara の標準出力は `raw / FV_SCALE` の整数値なので rshogi の `score` 列と照合する。
`--debug` が出す bucket と raw も各局面で一致することを検査する。
