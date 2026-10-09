# psv_to_hcpe3

`psv_to_hcpe3` は、YaneuraOu の PackedSfenValue（PSV, 40 バイト固定長）を
dlshogi の学習で使う **hcpe3** / **hcpe** 形式へストリーミング変換するツールです。

- 入力: PackedSfenValue 40 バイト固定長ファイル
- 出力: `--format hcpe3`（既定, 46 バイト固定長）または `--format hcpe`（38 バイト固定長）
- 特徴: 通常の着手レコードは cshogi 製変換スクリプトと **byte 完全一致**、チャンクストリーミングで
  ピークメモリを入力件数に非依存、スレッド数に依らず bit 一致

入力の move16 は実 YaneuraOu 形式 (A: bit14=駒打ち、bit15=成り) が前提です。
変換前に先頭10万レコードを検査し、旧リポジトリ形式 (B) または hcpe 形式 (C) の
確定シグネチャがあれば停止します。B 形式の既存 PSV は `migrate_psv_move16` で移行してください。

dlshogi の `train.py` は学習データに hcpe3（対局単位 + 候補手 visits）を要求しますが、
PSV は局面単位（棋譜構造を持ちません）。そこで各 PSV 局面を「1 局面 = 1 game」の
退化した hcpe3（`moveNum=1` / `candidateNum=1` / `visitNum=1`）として書き出します。
policy target は best move の one-hot、value は PSV の評価値から取ります。

`move16=0` は有効な着手を持たないレコードを表します。宣言勝ち終端のほか、局面置換によって
元の指し手が無効になった既存 PSV でも使われます。one-hot policy や hcpe の `bestMove16` を
構成できないため、このツールは hcpe3 / hcpe のどちらでも該当レコードをスキップし、破損
レコードとは分けて `有効な着手を持たないレコードをスキップ: N レコード (move16=0)` と
サマリ表示します。
着手なしを有効な候補手として出力すると replay と policy の規約を破るため、value だけを残す
独自表現には変換しません。

## 出力形式

局面の盤面は cshogi `Board.to_hcp` 互換の HuffmanCodedPos（32 バイト）で表現します。
指し手は hcpe 形式の move16 へ変換します。YaneuraOu PSV の move16 は
**bit14=駒打ちフラグ（from フィールド=駒種, 歩=1..金=7）・bit15=成りフラグ**で、
これを hcpe 形式（駒打ち `from = 81 + (駒種 - 1)`、成り `bit14`）へ意味的に復号します
（参照実装 cshogi `move16_from_psv` と一致）。勝敗は手番側視点 ±1/0 を
`0:DRAW / 1:BLACK_WIN / 2:WHITE_WIN` に変換します。

### hcpe3（既定, 46 バイト/レコード）

| フィールド | バイト | 値 |
|---|---|---|
| `hcp` | 32 | HuffmanCodedPos |
| `moveNum` | 2 (u16) | `1`（退化形） |
| `result` | 1 (u8) | 勝敗（0/1/2） |
| `opponent` | 1 (u8) | `0` |
| `selectedMove16` | 2 (u16) | best move（cshogi move16） |
| `eval` | 2 (i16) | PSV 評価値（`--evalfix-a` 指定時は焼き込み値） |
| `candidateNum` | 2 (u16) | `1` |
| `move16` | 2 (u16) | `selectedMove16` と同値 |
| `visitNum` | 2 (u16) | `1` |

### hcpe（38 バイト/レコード）

dlshogi 同梱 `psv_to_hcpe.py` 互換。`train.py` の `test_data`（検証データ）形式です。

| フィールド | バイト | 値 |
|---|---|---|
| `hcp` | 32 | HuffmanCodedPos |
| `eval` | 2 (i16) | PSV 評価値（`--evalfix-a` 指定時は焼き込み値） |
| `bestMove16` | 2 (u16) | best move（cshogi move16） |
| `gameResult` | 1 (u8) | 勝敗（0/1/2） |
| `dummy` | 1 (u8) | `0` |

## 使い方

```bash
# PSV -> hcpe3（dlshogi train.py 用、既定）
cargo run -p tools --release --bin psv_to_hcpe3 -- \
  --input data.psv --output train.hcpe3

# PSV -> hcpe（dlshogi test_data 用、38 バイト）
cargo run -p tools --release --bin psv_to_hcpe3 -- \
  --input data.psv --output val.hcpe --format hcpe

# 先頭 300 万件だけ変換し全コアを使う
cargo run -p tools --release --bin psv_to_hcpe3 -- \
  --input data.psv --output head.hcpe3 --limit 3000000 --threads 0
```

## オプション

| オプション | 既定 | 説明 |
|---|---|---|
| `--input` / `-i` | （必須） | 入力 PSV ファイル |
| `--output` / `-o` | （必須） | 出力ファイル。入力と同じ実体（hardlink を含む）や symlink は拒否 |
| `--format` | `hcpe3` | 出力形式（`hcpe3` / `hcpe`） |
| `--limit` | `0` | 処理レコード数の上限（0 = 無制限） |
| `--threads` | `0` | スレッド数（0 = 全コア） |
| `--chunk` | `200000` | チャンクサイズ（レコード数） |
| `--evalfix-a` | （未指定） | eval を `round_ties_even(score × 756.0865 / a)` で焼き込み ±32767 でクランプ（後述）。未指定なら生 score をそのまま書く |
| `--verbose` / `-v` | off | 変換できなかったレコードを逐次ログ |

## evalfix（`--evalfix-a`）

`--evalfix-a <a>` を指定すると、各レコードの `eval` を
`round_ties_even(score × 756.0864962951762 / a)` で焼き込み、`±32767` でクランプして
書き出します（丸めは python `round` 互換の round-half-even）。`756.0864962951762` は
dlshogi の固定 decode 定数 `1/0.0013226` で、`a` は学習側の eval スケールに合わせる係数です。
python 参照実装 `psv_to_hcpe_flat.py --evalfix_a` と **bit 一致**します。未指定時は生 score を
無加工で書きます（従来挙動）。`hcpe3` / `hcpe` どちらの形式の `eval` にも適用されます。

```bash
# eval をスケール a=1141.38 で焼き込んで hcpe へ変換
cargo run -p tools --release --bin psv_to_hcpe3 -- \
  --input data.psv --output val_ef.hcpe --format hcpe --evalfix-a 1141.381354386831
```

## スケーラビリティ・決定性

- 入力を `--chunk` 件ずつ読み、チャンク内を rayon で並列変換し、入力順のまま書き出す
  2 段ストリーミング。ピークメモリは `--chunk × (40 + 46) バイト` 程度で、入力件数に
  依存しません（数千万〜億局面規模を想定）。
- 出力はスレッド数・チャンク境界に依らず bit 一致します。
- **変換ホットパスはヒープ割り当てを行いません**。packed SFEN を SFEN 文字列・`Position`
  構築を経由せず、`unpack_sfen_to_parts` → `pack_hcp_from_parts` で直接 hcp へ展開します
  （CLAUDE.md「ホットパスでのヒープ割り当て禁止」）。文字列・`Position` 経由の旧実装に対し
  32 コア機で **約 30 倍**（10M 件 33s → 1.1s、system time 811s → 1s）になり、全コアが
  malloc/page-fault 競合でカーネルに張り付く問題を解消しています（出力は bit 一致を維持）。
- 変換できないレコード（壊れた PSV）や末尾の半端なバイト（レコード長未満）は
  スキップしてカウントし、正常レコードの出力バイト列には影響しません。出力は
  一時ファイル（`<output>.partial`）に書き、正常完了時のみ最終パスへ `rename`
  します（中断時も最終出力を部分データで置き換えません）。最終パスが未作成の場合は、
  **実行中は最終パスが存在しないのが正常**で、途中経過は `<output>.partial` を見ます。
- `<output>.partial` は排他的に新規作成します。既存の途中ファイルがある場合は変換を開始せず、
  入力・既存出力・途中ファイルを保持してエラー終了します。再実行前に、そのファイルの内容と
  他の変換が使用中でないことを確認し、移動または削除してください。
  I/O エラーや強制終了では `.partial` が残ることがあります。この場合も次回実行時には拒否します。
  出力と途中ファイルが入力と同じ実体（hardlink を含む）の場合や、symlink の場合も拒否します。
  途中ファイルがない場合、既存の通常出力ファイルは正常完了時に置き換えます。
- `move16=0` の有効な着手を持たないレコードもスキップしますが、入力破損ではないため変換エラー件数とは
  分けて集計します。ピークメモリと出力順の決定性には影響しません。
- 進捗表示は TTY では progress bar、非 TTY（background / リダイレクト）では
  数秒ごとのテキスト行（処理件数・rec/s・ETA）で出力します（background 実行が
  「無反応」に見えないようにするため）。

## bit 一致の検証

`tests/integration/psv_to_hcpe3_integration.rs` が、通常の着手レコードについて cshogi 製オラクル（`psv_to_hcpe3.py` /
dlshogi `psv_to_hcpe.py`）の出力と byte 完全一致することを検証します。実 YaneuraOu の
move16 形式で通常手・駒打ち・成りを含む 30 レコードの fixture
（`tests/fixtures/psv_to_hcpe3_yaneuraou_sample.*`）を使います。
旧 move16 形式の `psv_to_hcpe3_sample.psv` は、旧形式を拒否する回帰テストに使います。
