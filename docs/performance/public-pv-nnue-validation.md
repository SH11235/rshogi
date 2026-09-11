# 公開PV検証の本番NNUE追加測定（2026-09-12）

## 判断

PR #1082 の公開PV検証について、本番NNUEモデル、1/4スレッド、MultiPV 1/8、
中終盤の3局面で追加確認した。固定1秒の処理量に明確な悪化は観測しなかった。
固定ノードの指し手と探索指標も一致した。今回の範囲では追加の最適化を必要とする兆候はない。
これは公開用コピーの検証コストの確認であり、内部PV生成の修正や棋力測定ではない。

## 条件と再現

- 比較元: #1081 head `c6a89c5ed1d6429904ac1b5b6f1fcb7ec901b517`。
- 比較先: #1082 head `849e0358fa9870597ecccbb2f121d07ef057f5e7`。
- AMD Ryzen 9 5950X、rustc 1.95.0、production profile。1TはCPU 2、4TはCPU 2,3,4,5。
  CPU周波数は固定していない。両バイナリのビルドを済ませてから逐次探索した。
- 本番モデル `g50b-base@1200`、HalfKaHmMerged 1536x16x32、Material無効。
  モデルとprogress係数のSHA-256を照合し、NNUE読み込みと9バケットを起動ログで確認した。
- Hash 256 MiB、FV_SCALE 14、LS_BUCKET_MODE progresskpabs、LS_PROGRESS_BUCKETS 9。
  モデル名・ハッシュ・ビルド条件・バイナリハッシュは
  [public-pv-nnue-meta.json](public-pv-nnue-meta.json) に保存。
- complex-middle、movegen-heavyは既存性能測定の局面。
  selfplay-88は同モデルのbaselineを1T・1手3000ノードで動かした88手目の保存局面。
  最後の局面はSFENだけでなく全88手の履歴を渡す。正確な入力は
  [public-pv-nnue-positions.json](public-pv-nnue-positions.json) に保存。
- 各スレッド数でbaseline/fixedを常駐し、初期ウォームアップ後、各探索前に
  usinewgameとisreadyで初期化。モデル読み込み・初期化を時間から除外する。
  探索は重ねず、USI infoを別のreader threadで読み取り続ける。
- 固定ノードは1T・100,000 nodes、6ケース×ABBA 1ブロック（24探索）。
- 固定時間はgo movetime 1000、停止応答はgo infiniteの送信から250ms後にstop。
  それぞれ6ケース×ABBA 6ブロック×1/4T（各288探索）。合計600探索。
- 生データ: [public-pv-nnue-measurements.csv](public-pv-nnue-measurements.csv)。

両commitで次の条件でビルドし、実行ファイルを別名で保存する。

```sh
cargo build -j4 --profile production -p rshogi-usi --no-default-features \
  --features search-no-pass-rules,edition-layerstacks-halfka_hm_merged-1536x16x32-none
python3 scripts/bench_public_pv_usi.py BASELINE_BINARY FIXED_BINARY \
  "$SHOGI_DATA/nnue/ls-1536x16x32-halfka-hm-merged-wrm-n2s1200-gated50b-1200.bin" \
  "$SHOGI_DATA/progress/progress_hao_full_cuda.e1.bin" \
  docs/performance/public-pv-nnue-positions.json measurements.csv \
  --blocks 6 --movetime 1000 --stop-ms 250 --cpus 2,3,4,5
```

## 処理量と探索結果

| 固定1秒の条件 | 変更前 nodes/探索 | 変更後 nodes/探索 | ブロック平均変化率 | 参考95%区間 |
|---|---:|---:|---:|---:|
| 1T、MultiPV 1/8集計 | 718,510 | 733,110 | +1.97% | +0.95 ～ +3.00% |
| 4T、MultiPV 1/8集計 | 3,120,155 | 3,142,179 | +0.75% | -0.96 ～ +2.47% |

変化率はABBAブロックごとの合計nodesのfixed/baseline比の平均。
参考区間は6ブロックの比から求めたStudent t区間（df=5）。少数局面・周波数変動・
固定順序の探索的測定で、バイナリ配置などの影響も分離していない。速度向上とは断定しない。
MultiPV別の変化率は1Tで1: +2.36%、8: +1.63%、4Tで1: +0.96%、8: +0.56%。
4Tの個別ケースでは -0.40% ～ +2.00% の変化があり、SMPの探索順変動も含む。
固定時間の全288探索で950ms未満の早期終了はなく、詰み発見による短時間終了を
処理量悪化として集計したケースはない。

固定ノードの全24探索では各6ケース内でbestmove、最終infoのscore/depth/nodesが
変更前後・反復間で一致した。MultiPV 8ではscoreは最終info行の値であり、
全候補の評価値一致まで検査したものではない。
固定時間のSMPでは指し手・scoreの一致を条件としない。
公開PVの長さは変更前で最大30手、変更後で最大29手。1探索あたり最大105行の
探索infoを読み取り、既存の12手PVの単体測定より長いPVも含めて確認した。

## 終了・停止応答

時刻はPythonの単調時計で記録。送信直前からreaderがbestmoveを受信するまでを測る。
固定時間の値はその時間から1000msを引いた超過時間、stopの値はstop送信直前からの
応答時間。パイプ・OSスケジューリング・readerの遅延を含み、探索内部だけの時間ではない。
p95は標本分位点の線形補間で求めた。各条件・各版72探索。

| 条件 | 変更前 中央/p95/最大 (ms) | 変更後 中央/p95/最大 (ms) |
|---|---:|---:|
| 1T 固定時間超過 | 0.270 / 0.570 / 0.588 | 0.318 / 0.628 / 0.758 |
| 4T 固定時間超過 | 0.462 / 0.762 / 0.922 | 0.511 / 0.787 / 0.885 |
| 1T stop応答 | 2.438 / 2.924 / 3.016 | 2.337 / 2.844 / 3.091 |
| 4T stop応答 | 2.558 / 3.020 / 3.224 | 2.624 / 3.147 / 3.446 |

固定時間超過の中央値は約0.05ms増えた。4T stop応答のp95は約0.13ms増え、
標本最大は3.45msだった。追加処理のコストはゼロではないが、この条件では
実用上問題になる規模の遅延増加は観測していない。

固定時間超過は全件1ms未満だった。停止応答の少数標本から最悪時の上限は保証しない。

## 限界

本番モデルを用いた速度・応答測定を追加したが、Elo/SPRTは実施していない。
全時間制御（byoyomi、ponder等）、4を超えるThreads、8を超えるMultiPV、
全局面に対する無劣化も保証しない。内部PV生成やSEARCH-07の自然発生頻度を
この測定から評価することはできない。
PV全手の合法性は別の回帰テストで検証済みで、このUSI性能ハーネスは再生検証器ではない。
