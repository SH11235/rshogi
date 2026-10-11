# nyugyoku_metrics

`nyugyoku_metrics` は、評価関数が大差圏（勝ち切りに向かう終盤）で優劣の順序を正しく
出せるか（大差圏分解能）を、宣言ルール距離と通常探索の読み切り結果から測るツールです。
2 つの指標群を持ちます:

- **宣言ルール距離ペア順序一致**（`build-pairs` / `eval-pairs`）:
  `%KACHI`（入玉宣言勝ち）で終局した対局の勝者手番局面から「宣言成立へのルール距離が
  確定的に縮んだ」前後 pair を抽出し、NNUE 静的評価が後局面（距離小）を前局面（距離大）
  より高く評価するか＝順序一致率を条件別に測ります。
- **探索読み切り詰み距離 concordance**（`build-mates` / `eval-mates`）:
  `%TORYO`（投了）で終局した対局の終盤勝者手番局面を oracle 探索にかけ、詰みを
  読み切れた局面（mate in N）だけを採用し、静的評価が詰み距離の順序と整合するか
  （concordance）と、詰み手を全合法手中の最善候補に挙げられるか（詰み手 top-1 率）を
  測ります。現行の NNUE 学習は詰み圏 score を drop しており「詰みに近い局面ほど高く
  評価できるか」の物差しが無い、という欠落を埋める指標です。

既存の [ek_testset](ek_testset.md)（DT = 宣言可否 (ルール判定) / OC = 勝敗較正）が測れない
「大差圏内の順序分解能」を、評価対象の net の評価値を母集団選別や基準値に使わずに
測るのが目的です。

## 用語

- **mate（詰み）と宣言勝ちは厳密に区別します**。`build-mates` / `eval-mates` の mate は
  「詰み（checkmate）」のみで、入玉宣言勝ちを含みません。入玉宣言勝ちは宣言ルール距離
  ペア（`build-pairs` / `eval-pairs`）の担当です。両指標の母集団は終局特殊手
  （`%KACHI` / `%TORYO`）で排他に分かれます。
- 「勝者」= `%KACHI` では宣言側（終端で手番だった側）、`%TORYO` では投了した側の相手。
  抽出局面はいずれも勝者手番なので、評価値は同一 POV（手番相対）で直接比較できます。
  `'summary:` 行のある棋譜（wdoor の公開棋譜など）では、終局理由と勝者をその行から取ります
  （`toryo` → 投了、`kachi` → 宣言勝ち）。末尾が `%TORYO` でも `'summary:` が `abnormal` の対局は
  投了局に数えません。

## pair の定義と 4 条件

同一対局の勝者手番局面列から、**ply 差がちょうど 2 の隣接 pair**（間に相手の通常手が
1 手だけ挟まる並び）を取り、以下の遷移条件が 1 つ以上成立したものだけを出力します。
いずれも `Position::entering_king_point_info` / `in_check` によるルールベース判定です:

| 条件 | 判定 |
|---|---|
| `point_gain` | 宣言点（27点法の駒点）が増加 |
| `zone_piece_gain` | 敵陣三段内の駒数（玉を除く）が増加 |
| `king_entry` | 勝者玉が敵陣三段内へ入った（false→true） |
| `check_resolved` | 勝者玉への王手が解除された（true→false） |

間の手が parse fallback（`Move::NONE`）の pair や ply 欠番の pair は、盤面遷移を信頼
できないため出力しません。条件は排他ではなく、1 pair に複数付くことがあります
（`check_resolved` は宣言条件の王手回避と評価の王手ペナルティが交絡しうるため、
条件別の層のまま解釈してください）。また `point_gain` / `zone_piece_gain` の pair は、
同時に悪化遷移（被王手化 `check` false→true や玉の敵陣退出 `king_in` true→false）が
起きていても成立します。これらは除外せず、per-pair の `check_before/after` /
`king_in_before/after` フラグで post-hoc にフィルタして解析できます。

## build-pairs

```bash
cargo run -p tools --release --bin nyugyoku_metrics -- build-pairs \
  --input "$HOME/data/floodgate/extracted/kachi" \
  --out-dir runs/nyugyoku_metrics/kachi
```

CSA（1 ファイル = 1 対局）をファイル名ソートの決定的順序でストリーミング走査します
（ピークメモリは対局数に非依存）。各ファイルを一度だけ parse し、`%KACHI` 以外の終局を
スキップします。

`%KACHI` 終局でも、以下の対局は警告して除外します（除外数は `meta.json` に計上）:

- 宣言側と終端手番の突き合わせに失敗した対局（`games_skipped_winner_mismatch`）
- replay が完走せず終端局面を復元できない対局（`games_skipped_broken`）
- **終端局面で Point27 宣言が成立しない対局**（`games_skipped_point27_mismatch`）。
  wdoor shogi-server は宣言失敗（illegal kachi = 宣言側の負け）でも棋譜に `%KACHI` を
  書くため、最終手適用後の終端局面（宣言側手番）で `declaration_win`（27点法）を評価し、
  不成立の対局（宣言失敗・別ルール運用）を落とします。

出力:

| ファイル | 内容 |
|---|---|
| `pairs.jsonl` | 1 行 1 pair。`source_csa`, `winner` (`b`/`w`), `ply_before`, `ply_after`, `sfen_before`, `sfen_after`, `conditions`, `points_before/after`, `zone_before/after`, `king_in_before/after`, `check_before/after` |
| `meta.json` | 入力、走査対局数、`%KACHI` 対局数、除外数（上記 3 分類）、総 pair 数、条件別の pair 数と対局数（クラスタ数） |

## eval-pairs

```bash
cargo run -p tools --release --bin nyugyoku_metrics -- eval-pairs \
  --pairs runs/nyugyoku_metrics/kachi/pairs.jsonl \
  --eval-file "$SHOGI_DATA/nnue/model.bin" \
  --bucket-mode progresskpabs --progress-buckets 8 \
  --progress-file "$SHOGI_DATA/progress/progress.bin" \
  --out runs/nyugyoku_metrics/kachi/metrics.json
```

pair ごとに `sfen_before` / `sfen_after` を NNUE 静的評価（cp）し、順序一致
`eval_after > eval_before` を 1、tie を 0.5、逆転を 0 として、全体と条件別に
一致率・pair 数・対局数を集計します。標準出力と `--out` に同じ JSON を出します。
NNUE は `init_nnue` がロードできる全アーキテクチャ（LayerStacks、HalfKP 系など）に
対応します。

- **bucket-mode（LayerStacks 専用）**: LayerStacks では `--bucket-mode` が必須です。
  `progresskpabs` では `--progress-buckets` と `--progress-file` が必須です
  (`--progress-buckets 1` は常に bucket 0 を選ぶ no-op routing のため `--progress-file` 不要)。`--bucket-mode kingrank9` は
  progress 係数を使わないため `--progress-file` 不要です（指定するとエラーにします）。
  LayerStacks 以外の NNUE でこれらの routing option を指定すると
  エラーになります。
- **95% CI**: 対局クラスタ bootstrap（`source_csa` 単位の復元抽出、既定 `--bootstrap 10000`、
  `--seed 20260726`）で出します。分位点は replicate 統計量を昇順ソートし、0 始まりの
  index `round((R-1)·q)`（R = replicate 数、q = 0.025 / 0.975、round は四捨五入）の要素を
  採ります（nearest-rank 法 `ceil(R·q)-1` とは異なり、線形補間もしません。
  例: R=10000, q=0.025 → index 250）。条件別の bootstrap seed は
  `seed ^ (slot · 0x9E3779B97F4A7C15)` で派生します（全体 slot は seed のまま）。
  同 seed・同入力で結果は bit 一致します（`--bootstrap 0` で CI を省略）。条件ごとの
  クラスタ集合は「その条件の pair を 1 件以上持つ対局」です。実効クラスタ数（`n_games`）
  を必ず併記するので、クラスタが少ない層の CI は幅ではなく `n_games` を見て判断して
  ください。`pairs.jsonl` は対局数の事前集計と評価の2回、順次走査します。bootstrapの
  復元抽出回数を対局ごとに逐次生成するため、完了済み対局の集計はメモリに保持せず、
  ピークメモリは対局数ではなく `--bootstrap` に比例します。
- **fv_scale への非依存**: FV_SCALE は評価値の定数倍（狭義単調変換）にしか効かないため、
  順序一致率には影響しません（tie 潰れの量子化は測定対象の一部として残ります）。
- `--dump-pairs <path>` で pair ごとの `eval_before` / `eval_after` / `agreement` を
  jsonl 出力できます（deep-dive 用）。

出力 `metrics.json` の形:

```json
{
  "pairs": "...", "eval_file": "...", "progress_file": "...",
  "bucket_mode": "progresskpabs", "progress_buckets": 8, "bootstrap": 10000, "seed": 20260726,
  "overall":    { "agreement": 0.7, "n_pairs": 123, "n_games": 40, "ci95_lo": 0.6, "ci95_hi": 0.8 },
  "conditions": {
    "check_resolved":  { "agreement": ..., "n_pairs": ..., "n_games": ..., "ci95_lo": ..., "ci95_hi": ... },
    "king_entry":      { ... },
    "point_gain":      { ... },
    "zone_piece_gain": { ... }
  }
}
```

モデル間の比較は、同一の pairs.jsonl（同一 pair 集合）の上で行ってください。

## build-mates

```bash
cargo run -p tools --release --bin nyugyoku_metrics -- build-mates \
  --input "$HOME/data/floodgate/extracted/toryo" \
  --out-dir runs/nyugyoku_metrics/mates \
  --tail-plies 16 --stride 2 --oracle-depth 15 \
  --oracle-nodes 1000000 --threads 8
```

`%TORYO` 終局の対局（勝者 = 投了した側の相手）だけを対象に、終局側 tail の勝者手番局面を
oracle 探索へかけ、**詰みを読み切れた局面のみ**を `mates.jsonl` に出力します。CSA の走査は
`build-pairs` と同じファイル名ソートの決定的順序・streaming（対局単位処理）です。
`--max-games N` は走査順の先頭 N 対局で打ち切ります（`meta.json` に params が残るので
サブセット実行も再現可能）。

候補局面の選び方: 最終手（`%TORYO` 対局では勝者の手）の局面を距離 0 とし、終局からの
手数距離 `d` が `d < tail_plies` かつ `d % stride == 0` の勝者手番局面を候補にします
（勝者手番局面は 2 手ごとにしか現れないため、既定 `--stride 2` で tail 内の全勝者手番局面
= 既定 8 局面/対局が候補）。replay が完走しない対局（`Move::NONE` 混入等）と勝者の
突き合わせに失敗した対局は警告して除外し、`meta.json` に計上します。

### oracle 探索の設計

- **入玉宣言判定は必ず無効化**（`EnteringKingRule::None`）して探索します。
  EnteringKingRule 有効の探索は root の宣言可能局面を mate 帯スコア（`Value::MATE` +
  `Move::WIN`）で返すため、無効化しないと「宣言勝ち」が「詰み」として混入します。
  本指標の mate は詰みのみです（宣言は宣言ルール距離ペアの担当）。
- 採用条件は「探索 score が**手番側（勝者）の mate 帯**」のみ。自玉が詰まされる側の
  mate 帯（負側）や通常評価値の局面は採用しません。防御として、最善手が通常手でない
  結果（`Move::WIN` 等）と `Value::MATE` ちょうど（mate_ply 0 = root 宣言スコアの形）も
  採用しません。
- **`mate_in` は「探索が発見した詰み手数」であり最短の保証はありません**。固定
  `--oracle-depth` の探索は枝刈りを含むため、depth 内の詰みを見逃すこと
  （completeness の欠け）も、最短でない詰みを報告することもあります。同一
  mates.jsonl 上でモデル比較する限り、この限界は全モデルに共通で公平です。
- mate 帯 score は通常探索の読み切り結果です。TT が 16-bit key のため、理論上は衝突由来の
  偽 mate があり得ます。同一 mates.jsonl を使うことで oracle の差はモデル間に混入しない。
  ただし、偽 mate や非最短距離を含む可能性による指標自体の偏りは排除できない。
- oracle 探索は engine 既定の movegen（防御側の不成を生成しない）の上で読み切ります。
  不成のみが受けになる局面では偽 mate になり得ます。
- `--oracle-nodes N` は局面ごとの oracle 探索を最大 N nodes で打ち切ります（省略時は
  無制限）。`--oracle-depth` と併用でき、どちらかへ先に達した時点で打ち切ります。
  worst case の探索時間を抑えられる一方、上限を小さくするほど深い詰みの発見率が
  下がります。打ち切り結果が mate 帯 score でなければ、その候補は単に不採用になります。
- oracle は NNUE を要求せず、`MaterialLevel` Lv1 の駒得評価で探索します。このため
  mates.jsonl は **net 非依存の固定 oracle データ**になります（探索での詰みの発見率には
  使用する評価関数が影響し得ます）。
- 決定性: 局面ごとに `Search` を作り直し 1 スレッド固定で探索します（`teacher_labeler`
  と同じ不変条件）。`--threads` は局面単位の並列度で、出力はスレッド数に依存せず
  同一入力で bit 一致します。

出力:

| ファイル | 内容 |
|---|---|
| `mates.jsonl` | 1 行 1 局面。`source_csa`, `winner` (`b`/`w`), `ply`, `sfen`, `mate_in`（勝者手番から数えた ply）, `oracle_bestmove`（USI）, `oracle_depth`, `oracle_nodes`（無制限は `null`） |
| `meta.json` | params（`tail_plies` / `stride` / `max_games` / `oracle_depth` / `oracle_nodes`）、走査対局数、`%TORYO` 対局数、除外数（勝者不一致 / replay 未完走）、候補局面数、詰み読み切り数（採用数）、採用局面を持つ対局数 |

## eval-mates

```bash
cargo run -p tools --release --bin nyugyoku_metrics -- eval-mates \
  --mates runs/nyugyoku_metrics/mates/mates.jsonl \
  --eval-file "$SHOGI_DATA/nnue/model.bin" \
  --bucket-mode progresskpabs --progress-buckets 8 \
  --progress-file "$SHOGI_DATA/progress/progress.bin" \
  --out runs/nyugyoku_metrics/mates/metrics.json
```

mates.jsonl を NNUE 静的評価で採点し、2 指標を出します。NNUE は `init_nnue` がロード
できる全アーキテクチャに対応します。LayerStacks 専用の `--bucket-mode`・
`--progress-buckets`・`--progress-file` の扱いと対局クラスタ bootstrap（95% CI、分位点規約、決定性）は
eval-pairs と同一です。

1. **concordance**: 同一対局の mate 局面の全 pair（`mate_in` が厳密に異なる組合せのみ）に
   ついて、`mate_in` 小（詰みに近い）局面の静的評価が `mate_in` 大の局面より高ければ 1、
   tie 0.5、逆転 0。全局面が勝者手番なので手番相対 cp をそのまま比較します。
2. **詰み手 top-1 率**: 各 mate 局面で全合法手（`generate_legal_all`）を 1 手ずつ適用し、
   子局面を静的評価（相手番になるので符号反転して指し手側視点に揃える）。
   `oracle_bestmove` の値が全合法手中で厳密最大なら 1、最大タイに含まれるなら 0.5、
   それ以外 0。詰み手の子局面（相手玉に王手）も除外せず同じ規約で評価します。

bootstrap の対局クラスタは指標ごとに独立です（concordance = pair を 1 組以上持つ対局、
top-1 = mate 局面を 1 つ以上持つ対局）。concordance の bootstrap seed は `--seed` その
まま、top-1 は `seed ^ (1 · 0x9E3779B97F4A7C15)` で派生します。`n_games` が小さい層の
CI は幅ではなく `n_games` を見て判断してください。

`--dump <path>` で per-position / per-pair の明細 jsonl を出せます（行の `kind` が
`"position"` / `"pair"`）。

出力 `metrics.json` の形:

```json
{
  "mates": "...", "eval_file": "...", "progress_file": "...",
  "bucket_mode": "progresskpabs", "progress_buckets": 8, "bootstrap": 10000, "seed": 20260726,
  "concordance": { "agreement": 0.8, "n_pairs": 210, "n_games": 60, "ci95_lo": 0.7, "ci95_hi": 0.9 },
  "mate_top1":   { "rate": 0.4, "n_positions": 95, "n_games": 70, "ci95_lo": 0.3, "ci95_hi": 0.5 }
}
```

モデル間の比較は、同一の mates.jsonl（同一 oracle・同一局面集合）の上で行ってください。
