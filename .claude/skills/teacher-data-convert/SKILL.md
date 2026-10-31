---
name: teacher-data-convert
description: "教師データ（PSV: PackedSfenValue, 40B 固定長）を数千万〜数十億局面規模で一括変換・再評価するときの運用スキル。psv_to_hcpe3（PSV→dlshogi 学習用 hcpe/hcpe3、任意で eval 焼き込み）と rescore_psv（PSV を内部 NNUE / qsearch / 外部 USI エンジン / ONNX で再評価）を、スレッド設定・進捗確認・出力検証の落とし穴を避けて正しく回す。「PSV を hcpe に変換」「教師データを大量変換」「rescore で再評価」「大規模に局面をスコア付け」「hcpe3 を作る」「入玉の行だけ別のモデルで付け直した教師を作る」等で使用。"
user-invocable: true
---

# 教師データ一括変換・再評価スキル

`crates/tools` の 2 ツールで PSV（PackedSfenValue, 40B 固定長）を大規模処理する際の運用手順。
数千万〜数十億局面を前提とし、**スレッド設定・「ハングに見える」誤認・出力検証**の定番の罠を避ける。

対象ツール:
- **`psv_to_hcpe3`** — PSV → dlshogi 学習用 **hcpe3 / hcpe** に変換。任意で eval 焼き込み（evalfix）。
- **`rescore_psv`** — PSV を再評価してスコア/結果を付け直す。内部 NNUE 静的評価 / qsearch / 外部 USI エンジン / ONNX の各モード。

詳細なフラグ・形式は `crates/tools/docs/psv_to_hcpe3.md` / `crates/tools/docs/rescore_psv.md` を参照。

---

## ⚠️ 最初に押さえる罠

1. **両ツールとも `--threads` 既定は `0`（全コア）**。既定で全 CPU を使い切るため、
   **他の重い CPU 処理（例: DL 学習のデータローダ）と並走するときは、明示的に `--threads N` で
   上限を下げて譲る**こと（指定しないと全コアを掴んで他ジョブを圧迫する）。
2. **実行中の出力ファイルの状態はツールで異なる**（「出力が無い＝ハング」と誤認しない）。
   - `psv_to_hcpe3`: 処理中は `<output>.partial` に書き、正常完了時のみ最終名へ `rename`。
     **進捗は `<output>.partial` のサイズ増加で確認**。中断時は `.partial` を削除（壊れた最終出力を残さない）。
   - `rescore_psv`: `--output-dir` 配下の出力ファイルに**直接書き込み**（`.partial` は無い）。
     **進捗は出力ファイル本体のサイズ増加で確認**。中断・再実行時の扱いはモードで異なる（下表）。

     | モード | 完了マーカー | 再実行時 |
     |---|---|---|
     | NNUE 静的評価 / qsearch / 探索 / 外部エンジン | 書かない | 出力レコード数 ≥ 入力なら skip。途中までのファイルは最初からやり直す |
     | ONNX | `<出力名>.done` | fingerprint・サイズ一致なら skip、不一致なら truncate して再生成 |
     | `--out-scores`（ONNX / NNUE 静的評価） | `.in-progress` → 完了時 `.done` | fingerprint 一致なら既存 prefix へ追記して再開 |
3. **非 TTY（background / リダイレクト）では progress bar は出ない**が、`psv_to_hcpe3` は数秒ごとに
   テキスト進捗（件数 / rec/s / ETA）を出す。ログが「冒頭 1 行だけで止まって見える」のは正常。

---

## スレッド数の決め方

- **既定は `--threads 0`（全コア）**。出力はスレッド数に依らず bit 一致するので、
  決定的なラベル付け・変換はこのまま回す。
- 他の重い処理と並走するときだけ、物理コア数以下の `--threads N` に絞って余地を残す
  （NNUE 評価・局面構築は SIMD 主体で、SMT の論理コア領域では伸びが鈍い）。
  物理コア数は `lscpu` の `Core(s) per socket` × `Socket(s)`。
- **頼まれていない thread sweep を本番前に足さない**。所要時間の見積もりは本番の
  stderr 進捗（rec/s）から `総件数 / rec/s` で出す。

---

## psv_to_hcpe3（PSV → hcpe3 / hcpe）

```bash
cargo build --release -p tools --bin psv_to_hcpe3

# hcpe3（既定, 46B, dlshogi train.py 用）。既定で全コア使用。
./target/release/psv_to_hcpe3 --input "$SHOGI_DATA/teachers/<pool>.bin" \
  --output "$SHOGI_DATA/teachers/<out>.hcpe3"

# hcpe（38B, test_data 用）+ eval 焼き込み（evalfix）
./target/release/psv_to_hcpe3 --input "$SHOGI_DATA/teachers/<pool>.bin" \
  --output "$SHOGI_DATA/teachers/<out>.hcpe" --format hcpe --evalfix-a <a>
```

- `--evalfix-a <a>`: `round_ties_even(score × 756.0865 / a)` で eval を焼き込み ±32767 でクランプ
  （`756.0865` は dlshogi 固定 decode 定数。`<a>` のみが教師セット側のスケールに合わせる可変値）
  （ユーザーの private tooling にある参照 Python 実装の `--evalfix_a` と bit 一致）。**`<a>` は有限の正数のみ**（0/負/非有限はエラー）。
  未指定なら生 score をそのまま書く。`<a>` の値は教師セット側のスケールに合わせる。
- ストリーミング処理でピークメモリは入力件数に非依存（`--chunk` 件分のみ）。
- 出力はスレッド数・チャンク境界に依らず bit 一致する。
- 入力先頭（最大 10 万件）の move16 が旧形式だと、`migrate_psv_move16` での移行を促してエラー終了する。
- `<output>.partial` は `create_new` で作るため、異常終了（panic / kill 等）で残った `.partial` があると
  再実行が拒否される。中身と使用中でないことを確かめてから削除して再実行する。

## rescore_psv（PSV 再評価）

```bash
cargo build --release -p tools --bin rescore_psv

# 内部 NNUE 静的評価。既定は全コア。他ジョブと並走するなら --threads で上限を下げる。
# LayerStacks net は --ls-bucket-mode 必須（無いと起動時エラー）。progresskpabs は
# --ls-progress-buckets（格納 bucket 数と一致）と --ls-progress-coeff も必須。
./target/release/rescore_psv --input "$SHOGI_DATA/teachers/<pool>.bin" \
  --output-dir "$SHOGI_DATA/teachers/<out_dir>" \
  --nnue "$SHOGI_DATA/nnue/<model>.bin" \
  --ls-bucket-mode progresskpabs --ls-progress-buckets <bucket 数> \
  --ls-progress-coeff "$SHOGI_DATA/progress/<coeff>.bin" \
  --threads <物理コア数以下>
# kingrank9 の net は --ls-bucket-mode kingrank9 にして --ls-progress-* を外す

# qsearch 評価 / 葉ラベル等は --use-qsearch / --qsearch-leaf-label（docs 参照）
```

モード別の性質:
- **内部 NNUE 静的評価 / qsearch**: CPU 律速。PSV→局面の復元は文字列を経ず直接構築するため
  per-record のヒープ確保がなく、物理コア数までよくスケールする。**`--threads` の効果が最も大きい**のはここ。
- **外部 USI エンジン（`--engine`）/ ONNX（`--onnx-model` 等）**: GPU / 外部プロセス律速。
  CPU スレッド数の寄与は小さい（局面準備が支配的でない）。`--threads` を上げすぎると oversubscribe しうる。

---

## 大規模実行の運用

- **長時間ジョブは background 実行**（CLAUDE.md「長時間実行タスク」）。
- 進捗は出力サイズの増加（`psv_to_hcpe3` は `<output>.partial`、`rescore_psv` は `--output-dir` 配下の
  出力ファイル本体）、または stderr のテキスト進捗で監視。
- 物理 read が 0（`/proc/<pid>/io` の `read_bytes=0`）でも、入力先頭が page cache に載っているだけのことが多い。
  `rchar` と出力サイズが**同調して増えていれば正常な前進**。ループ疑いは「出力が増えず rchar だけ増える」ケース。
- 中断（Ctrl-C / SIGINT）時: `psv_to_hcpe3` は `.partial` を削除（壊れた最終出力を残さない）。
  `rescore_psv` は出力ファイルが中途残存し、再実行時の skip / やり直し / 再開はモードで異なる（冒頭の表）。

## 出力検証（必須）

大量変換・再評価は**出力の bit 一致を必ず確認**する:

```bash
# 参照実装やゴールデン出力との一致（先頭 N バイトを比較。N はバイト数）
# 例: 先頭 K レコードなら hcpe3=46B×K / hcpe=38B×K を N に指定
cmp -n <N> <out> <reference>

```

- 実装を変更した場合は、小さな入力で変更前後の出力が bit 一致すること（必要ならスレッド数を
  変えても一致すること、`sha256sum` で比較）を確認してからスケールさせる
  （一致しなければロジックが壊れている）。
- 破損レコード（不正な PSV）や末尾の半端バイトはスキップしてカウントされ、正常レコードの出力には影響しない。

reference の無い本番実行は、終了時の stderr と出力サイズで成功を判定する
（中断でも終了コードは 0 になり得るので、終了コードだけで判定しない）:

- **`psv_to_hcpe3`**: `<output>.partial` が残らず、最終名のファイルの mtime がこの run の開始より後
  (同じパスに前回 run の出力が残っている場合と区別する)。stderr の
  `書き出し: N レコード` の N に対し、出力サイズ = N × 46（hcpe3）/ N × 38（hcpe）。
  `--limit` なしなら、入力サイズを S として
  N + M + E = floor(S / 40) + (S mod 40 が 0 でなければ 1)
  が成り立つ (M = `有効な着手を持たないレコードをスキップ: M`、E = `注意: E レコードを変換できずスキップ`。
  末尾の半端バイトは E に 1 件として数えられる)。
  `完了前に中断されました` が出ていれば未完了。
- **`rescore_psv`**（PSV 出力。expand / leaf replacement 以外）: stderr 末尾が
  `All <n> file(s) processed successfully`（`Note: Processing was interrupted` なら未完了）。
  各出力のサイズが 40 の倍数で、`--limit` なしなら レコード数 = 入力レコード数 − `Note: E positions had errors`
  （ONNX は `Note: E 件のレコードでエラー`）の E − `Skipped (in check): S` の S。
  `--out-scores` は `<出力名>.done` があり、sidecar サイズ = 入力レコード数 × 2。
- E > 0 は入力側の破損を示す。件数を記録し、想定外に多ければ入力を調べてから学習に使う。

---

## 入玉の行だけを別のラベルで付け直した教師 (`psv_entered`)

入玉の行 (どちらかの玉が敵陣三段) は一般のラベラーの値が外れやすいので、そこだけ別のモデル (入玉向けに fine-tune した DL など) で付け直し、
dual-label PSV にして trainer の gated モード (入玉の行は base score、それ以外は DL score) で読ませる。手順と形式は
`crates/tools/docs/psv_entered.md`・`crates/tools/docs/psv_dual_label.md`。

```bash
# 1. 材料ごとに入玉の行を抜き出す
cargo run -p tools --release --bin psv_entered -- extract --input material.psv --out entered/material.entered.psv
# 2. 抜き出した行だけを付け直す (score だけを書き換えるモード。move16 や局面を書き換えるモードは 3 で一致しない)
cargo run -p tools --release --bin rescore_psv -- --input entered/material.entered.psv --output-dir relabeled/ <ラベラーの引数>
# 3. 元の材料と付け直した行から dual-label PSV を書く (元の score が DL でなければ --dl-scores に DL の sidecar)
cargo run -p tools --release --bin psv_entered -- merge-dual --base material.psv \
  --sparse relabeled/material.entered.psv --out dual/material.dual.psv [--dl-scores material.dl.i16]
# 4. 材料の順に partition して dedup とシャッフルを 1 段で (first-wins なので、重なった局面は前の材料の行が残る)
cargo run -p tools --release --bin psv_dedup_partition -- --partition-only --input "dual/<材料 A>/*.dual.psv" --temp-dir <tmp>
#    (材料ごとに --partition-only を順に重ねる)
cargo run -p tools --release --bin psv_dedup_partition -- --dedup-only --temp-dir <tmp> --output teacher.dual.psv --shuffle-seed <seed>
```

- **局面を組み立てる判定は CPU で律速する**。入玉の判定に `psv_gate_by_king_zone` のような 1 行ずつ局面を作る道具を挟むと、
  数百億行では読み書きより遅くなる。`psv_entered` は packed sfen の玉のマスだけを読むので、ディスクの速さで回る。
- **partition の一時領域は速いディスクに置く**。一時領域は入力と同じ大きさになり、HDD に置くと同じ HDD で読みと書きが重なる。
  出力と同じファイルシステムなら、`--keep-temp` なしの dedup は partition を 1 つずつ消しながら書くので、空きは入力の 1.05 倍で足りる
  (別のファイルシステムに出すときは、出力先に入力の 1.05 倍を求める)。`--keep-temp` なしで dedup が途中で止まると partition が欠けるので、
  partition からやり直す。
- **合わせる前の部品を残す**: 材料ごとの抜き出し・付け直し・DL の sidecar があれば、配合を変えたり別のラベラーで付け直したりするときに
  材料の読み直しだけで済む。
- 起動前に、小さな材料で旧手順 (または前回の出力) とバイト一致を確かめる (`cmp`)。`psv_dual_label validate` で
  entered bit と玉の位置の一致も見る。

---

## データ配置

教師データ / NNUE モデルは repo 外の共有 root `$SHOGI_DATA` に置く（CLAUDE.md「共有データ」）:
`teachers/`（PSV / hcpe 等）, `nnue/`（モデル）。tracked file・コマンド例には machine 固有の絶対パスを
書かず `$SHOGI_DATA` を使う。
