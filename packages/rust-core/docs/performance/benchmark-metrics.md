# ベンチマーク用メトリクス収集ガイド

このガイドでは、内蔵のTSVログとメトリクス解析ツールを使って、Ponder付与率、PV長、NPSへの影響を測定する方法を説明します。

## 1. ログ収集

- エンジンは `info string` にTSV形式の行を出力します。
- 代表的な行:
  - `kind=bestmove_metrics\t... pv_len=... ponder_source=... ponder_present=...`
  - `kind=bestmove_sent\t... nps=...`

実行例（ベストムーブを待ってからquitを送る安全な方法。bashのcoprocを使用）:

```
RUST_LOG=info bash -lc '
coproc E ( ./target/debug/engine-cli )
exec 3>&${E[1]} 4<&${E[0]}
printf "usi\nisready\nposition startpos\ngo depth 8\n" >&3
while IFS= read -r line <&4; do
  echo "$line"
  if [[ $line == bestmove* ]]; then
    printf "quit\n" >&3
    break
  fi
done
wait ${E_PID:-$COPROC_PID}
' > engine.log
```

注意: 単純なヒアドキュメントで `quit` を先に送ると、探索が即停止してメトリクスが取れないことがあります。

## 2. 解析

収集したログを解析します:

```
cargo run -p tools --bin metrics_analyzer < engine.log
```

出力内容:
- Samples: 測定サンプル数（bestmoveイベント数）
- Ponder rate: Ponderが付与された割合
- Avg PV length: bestmove時点の平均PV長
- Avg NPS: 送信時点の平均NPS

## 3. コツ
- 比較: 同一条件で前後比較し、解析結果を差分で評価します。
- TTを温める: 深めの探索や同一局面の繰り返しでTTを温めるとPonder率が上がります。
- 複数局面: 複数の実行ログを連結すれば、解析ツールが一括集計します。

