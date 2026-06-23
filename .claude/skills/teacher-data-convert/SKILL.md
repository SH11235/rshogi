---
description: 教師データ（PSV: PackedSfenValue, 40B 固定長）を数千万〜数十億局面規模で一括変換・再評価するときの運用スキル。psv_to_hcpe3（PSV→dlshogi 学習用 hcpe/hcpe3、任意で eval 焼き込み）と rescore_psv（PSV を内部 NNUE / qsearch / 外部 USI エンジン / ONNX で再評価）を、スレッド設定・進捗確認・出力検証の落とし穴を避けて正しく回す。「PSV を hcpe に変換」「教師データを大量変換」「rescore で再評価」「大規模に局面をスコア付け」「hcpe3 を作る」等で使用。
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
   上限を下げて譲る**こと（指定しないと全コアを掴んで他ジョブを圧迫する）。値の選び方は下記。
2. **実行中、最終出力パスにファイルは存在しない**。両ツールとも処理中は `<output>.partial`
   （rescore は `--output-dir` 配下）に書き、正常完了時のみ最終名へ `rename` する。
   「出力が無い＝ハング」と誤認しないこと。**進捗は `.partial` のサイズ増加で確認**できる。
3. **非 TTY（background / リダイレクト）では progress bar は出ない**が、`psv_to_hcpe3` は数秒ごとに
   テキスト進捗（件数 / rec/s / ETA）を出す。ログが「冒頭 1 行だけで止まって見える」のは正常。

---

## スレッド数の選び方（重要）

スループットを決める最大の要素。原則:

- **目安は「物理コア数」**。NNUE 評価や局面構築は SIMD（AVX2/AVX-512）主体で、
  **SMT（ハイパースレッド/論理コア）では伸びが鈍い**（実行ユニットを共有するため）。
  実測では物理コア数付近まで効率よくスケールし、論理コア領域はスループット増が小さく
  CPU 時間だけ膨らむ傾向。
- **マシンを専有できる**なら物理コア数（必要なら論理コア数まで上げて微増を取る）。
- **他の重い処理と並走する**（例: GPU 学習のデータローダが CPU を食う、別ジョブがある）場合は
  **物理コア数以下に絞る**と、自ジョブの効率を保ちつつ他へ余地を残せる。
- 物理コア数の確認: `lscpu` の `Core(s) per socket` × `Socket(s)`（`Thread(s) per core` が 2 なら SMT 有効）。

**「測定なしの最適化は禁止」（CLAUDE.md）に従い、本番前に自機で thread sweep を取る**のが確実:

```bash
# 代表サンプルで 1/4/8/16/… とスレッド数を振り、rec/s の飽和点を見る
for t in 1 4 8 16 32; do
  /usr/bin/time -v <tool> ... --threads $t   # wall / user / sys / rec/s を比較
done
```

スループットはマシン・モデル・モードで変わるため、得られた rec/s から
`総件数 / rec/s` で本番の所要時間を見積もる。

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

- `--evalfix-a <a>`: `round_ties_even(score × 定数 / a)` で eval を焼き込み ±32767 でクランプ
  （python 参照 `psv_to_hcpe_flat.py --evalfix_a` と bit 一致）。**`<a>` は有限の正数のみ**（0/負/非有限はエラー）。
  未指定なら生 score をそのまま書く。`<a>` の値は教師セット側のスケールに合わせる。
- ストリーミング処理でピークメモリは入力件数に非依存（`--chunk` 件分のみ）。
- 出力はスレッド数・チャンク境界に依らず bit 一致する。

## rescore_psv（PSV 再評価）

```bash
cargo build --release -p tools --bin rescore_psv

# 内部 NNUE 静的評価。既定は全コア。他ジョブと並走するなら --threads で上限を下げる。
./target/release/rescore_psv --input "$SHOGI_DATA/teachers/<pool>.bin" \
  --output-dir "$SHOGI_DATA/teachers/<out_dir>" \
  --nnue "$SHOGI_DATA/nnue/<model>.bin" --threads <物理コア数以下>

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
- 進捗は `.partial`（rescore は `--output-dir` 配下）のサイズ増加、または stderr のテキスト進捗で監視。
- 物理 read が 0（`/proc/<pid>/io` の `read_bytes=0`）でも、入力先頭が page cache に載っているだけのことが多い。
  `rchar` と出力サイズが**同調して増えていれば正常な前進**。ループ疑いは「出力が増えず rchar だけ増える」ケース。
- 中断（Ctrl-C / SIGINT）時は `.partial` を削除して終了するため、壊れた最終出力は残らない。

## 出力検証（必須）

大量変換・再評価は**出力の bit 一致を必ず確認**する:

```bash
# 参照実装やゴールデン出力との一致（先頭 N バイトを比較）
cmp -n <N> <out> <reference>

# スレッド数非依存の確認（同一入力・同一パラメータで threads を変えても一致すること）
sha256sum <out_threads16> <out_threads32>   # 一致するはず
```

- 実装を変更した場合は、変更前後で出力が bit 一致することを確認してからスケールさせる
  （一致しなければロジックが壊れている）。
- 破損レコード（不正な PSV）や末尾の半端バイトはスキップしてカウントされ、正常レコードの出力には影響しない。

---

## データ配置

教師データ / NNUE モデルは repo 外の共有 root `$SHOGI_DATA` に置く（CLAUDE.md「共有データ」）:
`teachers/`（PSV / hcpe 等）, `nnue/`（モデル）。tracked file・コマンド例には machine 固有の絶対パスを
書かず `$SHOGI_DATA` を使う。
