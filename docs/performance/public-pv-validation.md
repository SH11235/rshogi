# 公開PVの合法な接頭列への切り詰め（2026-09-11）

## 対象と判断

#1081 の head `c6a89c5ed1d6429904ac1b5b6f1fcb7ec901b517` を基準とした。
内部の RootMoves、previous_pv、探索・TT・評価処理は変更せず、Search::go の
途中の SearchInfo と最終 SearchResult に公開する PV だけを検証する。
完全な合法手集合と照合し、最初の不正手で打ち切る。PASS の権利を確認し、WIN は
適用中の入玉ルールで宣言できる場合だけ終端として保持する。
ponder_move は検証後の PV の2手目（WINを除く）から選ぶ。

これは内部 PV 生成の根本修正ではない。古い PV の残留などの内部原因は別件として残る。
出力に短い PV を返すことで、不確かな末尾を補う再探索は行わない。

今回の測定では、固定ノード・固定深さの指し手/score/深さ/ノード数は全ケース一致し、
固定時間の処理量に明確な悪化は観測しなかった。Material Lv1 の測定に限定した判断で、
NNUEモデルを使う実対局の Elo や、あらゆる局面・時間制御での無劣化を保証するものではない。

本番NNUE・Threads 1/4・MultiPV 1/8 の追加測定は
[public-pv-nnue-validation.md](public-pv-nnue-validation.md) を参照。

## 測定条件

- AMD Ryzen 9 5950X、Linux x86_64、rustc 1.95.0 (59807616e)。
- cargo test --release、既定features、target-cpu=native、同じテストハーネス。
- 1T は CPU 2、2T は CPU 2,3 に taskset で固定。周波数は固定していない。
- Material Lv1、Hash 16 MiB、各探索で新しい Search。
- 平手、6手の相掛かり、12手の横歩取り形の3局面、MultiPV 1/4（計6ケース）。
  正確な指し手列は `crates/rshogi-core/tests/public_pv_bench.rs` に記載。
- callback は情報を black_box に渡す。端末へのUSI出力時間は含めず、検証・callback
  呼出を含む Search::go の実時間を測る。
- 100,000 nodes、depth 10、movetime 200ms を比較。ノード制限の既存超過分も記録する。
- baseline/fixed/fixed/baseline（ABBA）を8ブロック、depthのみ4ブロック。
- 正確な生データは [public-pv-measurements.csv](public-pv-measurements.csv)（672探索）。
  変更前後の実行ファイルを別々に保存し、ビルド終了後に測定した。

## 結果

| 条件 | 変更前 | 変更後 | ブロック平均の変更率 | 参考95%区間 |
|---|---:|---:|---:|---:|
| 100,000 nodes / 1T：ms/探索 | 114.011 | 113.671 | -0.30% | -0.92 ～ +0.33% |
| depth 10 / 1T：ms/探索 | 80.057 | 79.675 | -0.48% | -0.90 ～ -0.05% |
| 200ms / 1T：nodes/探索 | 223,117 | 223,570 | +0.21% | -0.37 ～ +0.78% |
| 200ms / 2T：nodes/探索 | 424,763 | 426,507 | +0.41% | -0.14 ～ +0.96% |

変更率は各ABBAブロック内の fixed/baseline 比の平均。参考区間はその比に対する
Student t 区間（8ブロックはdf=7、4ブロックはdf=3）。短時間・少数局面・周波数変動のある
探索的測定なので、速度改善の根拠にはしない。固定ノード/深さでは全288探索を通じ、
同じケースの bestmove/score/depth/nodes は変更前後・反復間で一致した。
固定時間の2Tでは探索順も変動するため、指し手やscoreの一致は要求しない。

### 検証単体のコスト

公開用PVをコピーして検証する操作を各20,000回測定。
[public-pv-cost.csv](public-pv-cost.csv) にコピーのみの対照も含めた。

| PVの長さ | 履歴0手 | 履歴512手 |
|---|---:|---:|
| 6手 | 約2.9 µs/回 | 約5.6 µs/回 |
| 12手 | 約6.2 µs/回 | 約9.0 µs/回 |

局面コピーは履歴長に依存する。512手の履歴は可逆な玉移動を繰り返して構築し、
この測定では千日手の探索を行わず、合法手検証の単体コストだけを測った。
全体比較の100,000 nodesでは平均約27回のcallbackがあり、微小な追加コストはある。
単体測定と全探索測定から、今回の構成では公開PVを安全化するコストを許容できると判断する。

## 再現

変更前worktreeにも同じ `crates/rshogi-core/tests/public_pv_bench.rs` を配置し、両方で実行する。
ビルドが出力する `Executable ...` の実行ファイルを別々に保存する。

```sh
cargo test -j4 --release -p rshogi-core --test public_pv_bench --no-run
python3 scripts/bench_public_pv.py BASELINE_BINARY FIXED_BINARY measurements.csv --cpus 2,3
cargo test -j4 --release -p rshogi-core --lib public_pv_validation_cost -- --ignored --nocapture
```

BASELINE_BINARY/FIXED_BINARY は保存した実行ファイルのパスに置き換える。
単体コスト測定は変更後でのみ実行する。通常の cargo test では両ベンチを実行しない。

## 回帰検証

- 再現済みの重複着手、NONE、不正なWIN/PASS、手番違いの手を打ち切る。
- 正常なPV、宣言勝ち、権利内のPASSを保持し、元の局面/PVを変更しない。
- callback有無で固定ノード探索の結果と内部RootMoves/previous_pvが一致する。
- Threads 1/2 × MultiPV 1/4 で途中infoと最終PVの全手を合法手集合と照合して再生する。
- callbackなしの最終結果と、終局/宣言勝ちの結果も検証する。
