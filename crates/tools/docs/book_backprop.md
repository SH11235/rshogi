# book_backprop

`book_backprop` は YANEURAOU-DB2016 テキスト定跡 `.db` の候補手評価値を、book 内の子局面から negamax で親方向へ逆伝播するツールです。`count` / `ponder` / `depth` / 局面構造は保持し、`value` だけを更新します。

## 使い方

```bash
cargo run -p tools --release --bin book_backprop -- \
  --book in.db \
  --out out.db \
  --draw-value 0 \
  --merge min \
  --report report.md \
  --max-iters 1000
```

必須オプション:

| オプション | 説明 |
|---|---|
| `--book <PATH>` | 入力 `.db` |
| `--out <PATH>` | 出力 `.db` |

任意オプション:

| オプション | 既定値 | 説明 |
|---|---:|---|
| `--draw-value <CP>` | `0` | 循環 SCC の千日手値。手番側視点 |
| `--merge <MODE>` | `min` | book 内子局面からの伝播値と既存ラベル値の合成。`min` または `replace` |
| `--report <PATH>` | なし | Markdown レポートの出力先 |
| `--max-iters <N>` | `1000` | 非自明 SCC の値反復ガード。到達時はエラー終了 |

## 伝播規則

局面キーは SFEN の ply を除いた board / turn / hands の 3 フィールドです。出力の `sfen` 行には入力側の ply を残し、同一キーが複数ある場合は最小 ply の行を代表として使います。

各 book 手は次の規則で更新します。

| 子局面 | 伝播値 |
|---|---|
| book 内に直接存在 | `propagated = -best(子)` |
| 直接 miss だが先後反転キーが book 内に存在 | `propagated = -best(反転子)` |
| book 外 | 既存 `value` を維持 |
| 非合法手 | stderr に警告し、既存 `value` を維持 |

歩・香の最終段、桂の最終二段への打ち・不成も非合法手として除外します。子局面がbook内に存在していても、その手の既存 `value` を維持します。合法な不成は通常の手と同様に逆伝播します。

`best(N)` は局面 `N` の候補手 `value` の最大値です。book 内子局面が見つかった手の最終値は `--merge` で決まります。

| mode | 更新 |
|---|---|
| `replace` | `value = propagated` |
| `min` | `value = min(既存ラベル値, propagated)` |

既定の `min` は値を下げる方向にだけ伝播します。probe 時の相手は自分の book 内候補に制限されないため、子局面での相手の取り分は、既存ラベル値が持つ制限なし探索の見積りと book 内 best の大きい方以上です。したがって親手の値は、既存ラベル値と `-best(子)` の小さい方を上界として扱います。

出力は決定的で、局面は SFEN key 昇順、手は `count` 降順から USI 昇順で書き出します。

## 循環 SCC

ply を除いた局面グラフは千日手相当の循環を持つことがあります。`book_backprop` は SCC を縮約し、縮約 DAG を子側から処理します。

非自明 SCC では、SCC 外への手は確定済みの `-best(子)` とし、SCC 内に留まる手は `--draw-value` を下界として値反復します。`--merge min` ではこの伝播値をさらに既存ラベル値との `min` で合成するため、book 内辺の値は単調非増加です。値集合は既存の葉値、SCC 外の確定値、`draw-value` とその negamax 合成に限られるため有限で、不動点に達すると停止します。`--max-iters` に達した場合は実装バグまたは入力条件の見直しが必要な状態としてエラー終了します。

## レポート

`--report` を指定すると Markdown で以下を出力します。

| 項目 | 内容 |
|---|---|
| Summary | merge mode、ノード数、手数、更新手数、book 内辺数、flip 合流辺数、非合法手数 |
| Value deltas | `|Δ|` の p50 / p90 / max とヒストグラム |
| Propagation depth | 縮約 DAG で葉から何段伝播したかの分布 |
| SCC | 非自明 SCC 数、最大サイズ、draw-value になった SCC 内手数、値反復回数 |
| Top changed nodes | 旧 best と新 best の差が大きい上位 20 局面 |
