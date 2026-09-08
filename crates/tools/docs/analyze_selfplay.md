# analyze_selfplay

`tournament` 等が出力した JSONL を読み、エンジン別勝敗、直接対決、Elo/nElo、
NPS や深さなどの追加統計を表示する。

## 使い方

```bash
./target/release/analyze_selfplay runs/selfplay/{DIR}/*.jsonl
```

SPRT の post-hoc 判定も表示する場合:

```bash
./target/release/analyze_selfplay --sprt runs/selfplay/{DIR}/*.jsonl
```

`--sprt-base-label` / `--sprt-test-label` で役割を明示できる。省略時は SPRT meta、
`base_label` 記録、ラベル名などから推定し、推定根拠を標準エラー出力に表示する。

Wald パラメータは `--sprt-nelo0` / `--sprt-nelo1` / `--sprt-alpha` /
`--sprt-beta` で上書きできる。

## 表示の視点

- エンジンの `A(...)`, `B(...)` ラベルはエンジン ID の辞書順で決まる。
- 通常の直接対決は辞書順で左側のエンジン視点で、勝率と Elo/nElo の符号を表示する。
- `--sprt` 時の SPRT 対象ペアは `test vs base` の順に表示し、直接対決と SPRT
  レポートをどちらも test 視点に揃える。
- JSON 出力の `head_to_head` は互換性のため従来どおり辞書順を維持する。SPRT の
  `test` / `base` および統計値は test 視点である。

`nElo` と SPRT の pentanomial 集計は、同じ開始局面を先後入れ替えた 2 局を
1 ペアとして扱う。`pair_index` と `attempt` が同じ 2 局だけを組にし、error を含む
世代は正常終了した相方も含めて勝敗・直接対決・SPRT から除外する。片スロットしか
完了していない世代も除外し、件数を情報表示する。`attempt` が無い旧ログは 0 として扱う。

追加統計には `error局`、`errorペア`、`再試行ペア`、`枯渇ペア` を表示する。
`枯渇ペア` が 1 以上なら、そのテストはインフラ障害により invalid である。

## 終局理由

「終局理由」には `reason` 別の件数と、入力ファイルを通じた全 result 行数に対する
割合を表示する。error 局・再試行前の世代・重複行・未完了ペアの行も各 1 件として数える。
これは終局状況の監視用であり、WLD / SPRT のペア除外条件は変わらない。
`error` で始まる reason は `error` にまとめ、reason の無い旧ログは `unknown` とする。

表示順は `resign`, `win`, `sennichite`, `sennichite_perpetual_check`,
`adjudication_resign`, `adjudication_draw`, `max_moves`, `timeout`, `illegal_move`,
`no_bestmove`, `error`、続いてその他の理由を辞書順とし、0 件は省略する。
`max_moves 到達率` は 0 件でも別行で明示する。result 行の無い入力ではこのセクションは表示しない。
`--json` では `extra.reasons` に `{ "理由": 件数 }` を出力する。

`sennichite` / `adjudication_draw` / `max_moves` は通常の引分として、
`sennichite_perpetual_check` / `adjudication_resign` は勝者の勝ちとして集計する。
