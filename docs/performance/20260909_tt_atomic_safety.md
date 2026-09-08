# TT atomic snapshot / writer lifetime 検証（2026-09-09）

## 変更と契約

共有テーブルは AtomicU16 × 5 の10バイト格納を使い、3エントリと1バイトの
Acquire/Release排他、1バイトのpaddingを32バイトclusterに収める。公開TTEntryは
従来どおり所有されたsnapshotで、置換条件の計算に使用する。

probe/read/hashfull/writeは短いcluster排他の内側でsnapshotを読み書きする。
probe結果はロックを持ち越さず、write時に最新snapshotから置換条件を再評価する。
競合中のprobeはmiss（そのwriterも書込み省略）、writeは省略する。spin待機しないため
探索の停止応答をロック待機に依存させない。競合によってcache利用と探索木は変わり得る。
new_searchの世代は独立atomicを維持する。clear/resizeは排他借用を要求し、
ProbeResultはテーブル借用に結び付くため、生存writerと共存できない。

## 検証

- 通常probe/write、3slot容量、depth-age置換、世代、clear/resize。
- stale writerが別keyへ置換されたslotを再評価し、同じkeyの新しい深い結果を保存条件で保護。
- 2writer + 2readerの同一cluster負荷でkey/value/eval/depth/bound/PV/手の整合性。
- 排他取得失敗時のmiss/書込み省略、解放後の利用再開。
- drop/resize後のwriter利用を拒否するcompile-fail doctest 2件。
- ThreadSanitizerでTT関連18テストを通過（sanitizer対象外の実対局全体の保証ではない）。

```sh
cargo test -p rshogi-core --lib tt::
cargo test -p rshogi-core --doc
RUSTFLAGS=-Zsanitizer=thread CARGO_TARGET_DIR=target-tsan \
  cargo +nightly test -Zbuild-std --target x86_64-unknown-linux-gnu \
  -p rshogi-core --lib tt::
```

Sanitizer実行環境: rustc 1.95.0-nightly (c04308580 2026-02-18)、rust-src導入済み。
全workspaceのcargo test、Clippy、fmt、tracked絶対パス検査も通過。

## 限定した固定時間比較

baseline: `0d4c3b644`、比較対象: 本PRのatomic/lifetime修正。
AMD Ryzen 9 5950X、Linux x86_64、rustc 1.97.1 (8bab26f4f 2026-07-14)。
同じrelease設定（LTO off / codegen-units 16、target-cpu=native）で両方をbuildした。
独立processごとにMaterialLevel=9、USI_Hash=16、Threads=1または2を設定。
各局面でbase→patch→patch→baseの順に`go movetime 1000`。
各探索の最終infoのnodesを足した値を示す。CPU固定・周波数固定・専有はしていない。

| Threads | base nodes（6探索合計） | patch nodes（6探索合計） | patch/base |
|---|---:|---:|---:|
| 1 | 3,864,926 | 3,747,142 | 0.9695 |
| 2 | 7,374,983 | 7,079,358 | 0.9599 |

この短いMaterial評価での比較では約3–4%のnodes減少を観測した。
本番NNUE、長い持時間、多数スレッド、棋力の採否を示す測定ではない。
固定時間SPRT等の正式な採否検定は実施していない。

使用局面（USI position引数）:

```text
startpos
startpos moves 7g7f 3c3d 2g2f 8c8d 2f2e 8d8e 2e2d 2c2d
sfen 8l/1l+R2P3/p2pBG1pp/kps1p4/Nn1P2G2/P1P1P2PP/1PS6/1KSG3+r1/LN2+p3L w Sbgn3p 124
```

別にThreads=1、同じ3局面を`go depth 5`で比較すると、nodesは順に2009 / 546 / 6189で一致し、
score・PV・bestmoveも一致した。これは決定性回帰の確認で、速度・棋力の証拠には使わない。

## 競合頻度の限定観測

事前にkey1/2を同じclusterに保存し、2writer + 2readerがその2keyだけを10万回ずつprobeする
独立harnessでは、400,000 probeのうち367,345（91.84%）が排他競合でmissした。
writer側のprobe missは184,640で、これらはwriteも省略される。probe成功後のwrite時競合は
別途数えておらず、書込み省略数は下限のみ。実探索の競合頻度は未計測。
これは単一clusterに負荷を集中させた意図的な極端条件であり、通常のcache hit率を表さない。