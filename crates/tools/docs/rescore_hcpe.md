# rescore_hcpe — hcpe 教師の eval を NNUE 固定 depth 探索で付け替え

hcpe（cshogi HuffmanCodedPosAndEval, 38B/レコード）教師プールの各局面を **NNUE 固定 depth
探索**で再評価し、**eval だけを差し替えた hcpe** を出力する教師生成ツール（局面・bestMove16・
gameResult は保持）。`yardstick_label`（ラベル品質の物差し）と**共有コア `teacher_labeler` を
経由**するため、同一 config（net / fv-scale / progress 係数 / SPSA params / depth / hash）なら
両者のラベルは **bit 一致**する（「測った config = 回す config」）。

## 特徴

- **fresh-per-position の決定的ラベル**: 局面ごとに空の `Search` を作るため、TT/history が
  局面間で持ち越されない。処理順・スレッド数・入力分割（シャード）に依存せず、同一局面は常に
  同一ラベルになる → **複数機で分散ラベリングしてシャードを連結できる**。
  （`rescore_psv --search-depth` は worker ごとに `Search` を使い回すため TT/history が持ち越され、
  この用途には使えない。`rescore_hcpe` は共有コアの fresh-per-position でこれを回避している。）
- **resume（チャンク単位 + チャンク途中）**: 入力をチャンクファイル群で渡し、`--out-dir` に
  入力ファイル名で出力する。各出力には完了メタ `<出力名>.meta`（入力サイズ・出力件数・config
  指紋）を `.tmp` → rename で原子的に書く。resume 時は **メタと出力サイズが一致するチャンクだけ
  skip**（破損・短縮出力・設定違い・`--limit` 短縮を「完了」と誤認しない）。GPU 学習等で中断 →
  同じコマンドで再実行すると未処理チャンクから再開する。
  - **intra-chunk resume**: 処理中チャンクは `.tmp` に書けた連続プレフィックスをサイドカー
    `<出力名>.tmp.meta`（config 指紋・入力サイズ・入力 seq・出力件数の checkpoint）で裏取りし、
    **チャンク途中から再開**する。checkpoint は数百レコードごとに `.tmp` を flush + fsync して
    から原子的に更新するため、「checkpoint が指す出力件数 ≤ disk 上の `.tmp` レコード数」が常に
    成立し、電源断でも checkpoint を超えて再開しない。fresh-per-position の決定性ゆえ途中再開でも
    全件フレッシュ処理と **bit 一致**する。これにより中断/電源断時の損失は最悪でも checkpoint
    間隔（数百レコード ≒ 数秒）に収まる。
  - `.tmp.meta` が無い（旧バイナリ残置）・config / 入力サイズ不一致・`.tmp` が checkpoint 分に
    満たない（torn write）等、少しでも矛盾すれば `.tmp` を信頼せず最初から処理する（後方互換・
    安全側）。完了 rename 後は `.tmp.meta` を削除する。
  - `--overwrite` 指定時は config 指紋が一致しても残存 `.tmp` を信頼せず最初から処理する。config
    指紋はバイナリのコード変更を捕捉しないため、探索コード修正後などに同一 config で再ラベルを
    強制する際、旧 prefix と新 suffix が混在した出力を完了扱いするのを防ぐ。
- **入力 basename は一意必須**: 出力は入力ファイル名で書くため、別ディレクトリでも同名チャンクが
  あると出力が衝突して silent にチャンクが欠落する。重複 basename と予約サフィックス（`.tmp`/
  `.meta`）は起動時にエラーで弾く。
- **破損レコードは fail-loud**: unpack/set_sfen 失敗レコードがあるとそのファイルは rename されず
  未完了のまま（黙ってレコードを落として「完了」にしない）。最後に非ゼロ終了し、修正後の再実行で
  resume される。
- **config を変えたら別 `--out-dir`**: resume は config 指紋（net/係数/fv/depth/hash/clip/SPSA 等）
  一致を要求するので、設定を変えるときは出力先を分けるか `--overwrite`。
- **streaming**: ファイルごとに in-flight をトークンで上限管理するため、ピークメモリは入力
  サイズ非依存。
- 符号規約は手番側視点 cp（hcpe 保存 eval と同じ）。探索値は `--score-clip` で i16 に収める。

## 使い方

```bash
# 例: floodgate+aoba のチャンク群を NNUE@d15（SPSA params）で再ラベル → teacher 出力
rescore_hcpe \
  --in 'pool/chunk_*.hcpe' \
  --out-dir teacher_d15/ \
  --nnue "$SHOGI_DATA/nnue/ls_halfka_hm_merged_1536x16x32_none/nnue_train_allfp16-r3-400.bin" \
  --fv-scale 28 \
  --ls-progress-coeff "$SHOGI_DATA/progress/progress_hao_full_cuda.e1.bin" \
  --spsa-params spsa_params/v99-400-suisho10.rshogi.params \
  --depth 15 --nodes 0 --hash-mb 32 --threads 0
```

中断後の再開は**同じコマンドを再実行**するだけ（`teacher_d15/` に出来ているチャンクは skip）。

## オプション

| フラグ | 既定 | 説明 |
|---|---|---|
| `--in <PATH>...` | （必須） | 入力 hcpe（38B/レコード）。複数指定・glob（例 `pool/*.hcpe`）可。出力済みを除き決定的順序で処理 |
| `--out-dir <DIR>` | （必須） | 出力先。入力ファイル名と同名で hcpe を書く（= resume の単位） |
| `--nnue <PATH>` | （必須） | labeler の NNUE モデル |
| `--fv-scale <i32>` | 0 | FV_SCALE（0=ヘッダ自動、none/threat LayerStacks 系は 28） |
| `--ls-bucket-mode <STR>` | — | LayerStacks bucket mode（LS ビルドでは既定なので通常不要） |
| `--ls-progress-coeff <PATH>` | — | progress8kpabs 用係数（LS + progress8kpabs で必須） |
| `--spsa-params <PATH>` | — | SPSA 探索 params（USI `SPSAParamsFile` 同形式）を各局面の探索へ適用。未指定は engine 既定値 |
| `--depth <i32>` | 15 | 探索深さ（固定 depth ラベリング） |
| `--nodes <u64>` | 0 | 探索ノード上限（0=無制限）。depth を binding にするなら 0 |
| `--hash-mb <usize>` | 32 | worker ごとの置換表サイズ（MB）。局面ごとに作り直すため過大にしない |
| `--threads <usize>` | 0 | worker スレッド数（0=全コア）。出力は thread 数非依存に bit 一致 |
| `--score-clip <i32>` | 32000 | 出力 eval を ±この値に clamp して i16 へ収める |
| `--skip-in-check` | false | 王手局面を出力から除外 |
| `--limit <usize>` | 0 | ファイルごとの先頭最大レコード数（0=全件）。smoke 用 |
| `--overwrite` | false | 完了済み出力も処理中 `.tmp` も無視して最初から再処理（既定は skip = resume） |

## 関連

- `yardstick_label` / `yardstick_score` — ラベル品質の物差し（同じ `teacher_labeler` コアを使う）。
  本ツールで作る teacher のラベルは yardstick の measure と bit 一致する。
- `rescore_psv` — PSV 版の再スコアリング（ONNX/GPU・qsearch-leaf 等の機能が豊富）。format が
  hcpe ではなく PSV で、`--search-depth` は fresh-per-position ではない点が異なる。
