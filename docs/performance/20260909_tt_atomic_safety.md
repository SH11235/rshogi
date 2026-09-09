# TT relaxed atomic / writer lifetime 検証

## 設計と契約

`Cluster` は `data: [AtomicU64; 3]`、`keys: [AtomicU16; 3]`、
2バイトのpaddingを持つSoA配置。32バイト境界とサイズのcompile-time assertを維持し、
1 MiB当たり32,768 cluster / 98,304 entryの容量は変わらない。
所有snapshotの `TTEntry` は10バイトのまま。チェックサム方式は使用しない。

payloadの下位からdepth8、generation/PV/bound8、move16、value16、eval16を
64bitへ符号化する。符号付き値はu16経由でビット列を保持する。
probe/hashfull/writeはRelaxedでアクセスし、snapshotのload/storeはそれぞれ
5回のAtomicU16アクセスからAtomicU16とAtomicU64の2回になる。
writeは再読込と格納を行うため合計4回。クラスタロック、待機、競合によるskipはない。
payload内の対応は単一store/loadで保たれるが、keyとの一貫性は保証しない。
probeは所有値のsnapshotを返し、writerは常に借用したclusterとslotを持つ。
生ポインタからの可変entry参照は作らない。clear/resizeは排他借用を要求し、
drop/resize後のwriter使用を拒否するcompile-fail doctestを維持する。

probeの置換候補は従来の `depth8 - relative_age`、writeはslotを再読込して
`TTEntry::save` のExact・異key・depth/PV・generation条件を適用する。
条件式と同一keyの手の保持規則は変更していない。ただし並行writer間での
判定と格納は不可分ではなく、古いsnapshotの再格納や深い結果の上書きは起こり得る。
これはロックによる直列化を外すことに伴う制約である。

`write -> bool` と `#[must_use]` は維持する。falseになる条件はなく、trueは
「置換条件を適用した格納操作が完了した」を表す。値の変更や、その後の上書きがないこと、
複数wordの同時公開を意味しない。置換条件で既存値を保持してもtrueなのは従来と同じ。
戻り値は現在の実装では情報量がなく冗長だが、成否APIと呼出側の記録契約を維持するため残す。
`alpha_beta.rs` / `eval_helpers.rs` / `pruning.rs` / `qsearch.rs` の成功時のみ
統計・トレースを計上する分岐は維持。記録は格納操作の入力であり、後続probeの観測値を保証しない。

## torn entryの防御と限界

**メモリ上のdata raceを防ぐことと、探索結果が影響を受けないことは別である。
この設計でtorn entryが探索に無害とは保証できない。**

実際のコード上の検証経路:

- `tt/table.rs::probe`: 読み取った `key16` と検索keyの下位16bitを照合する。
  異なる短縮キーは除外するが、keyと他wordの対応は検証しない。
- `position/pos.rs::to_move`: `Move::from_u16_checked` で符号化を検証し、
  通常手の移動元の駒・手番・成れる駒種を確認する。不整合ならprobeはそのslotを除外する。
  移動規則、打ち駒の持ち駒、自玉の安全までは検証しない。`Move::NONE` は検査対象外。
- `search/movepicker.rs` の通常・王手回避・qsearch・ProbCut用初期化で
  `pseudo_legal_with_all` を適用する。`alpha_beta.rs` の探索ループは
  `pseudo_legal` と `is_legal`、`qsearch.rs` と `pruning.rs` のループは
  `is_legal` を実行してから着手する。この経路には合法性検査が実在するため追加しない。
- `search/eval_helpers.rs` と `qsearch.rs` のTT cutoffは、その着手検査より前に行われる。
  `tt_sanity.rs` のvalue/eval範囲検査は範囲外を弾くだけで、別探索由来の有効値は弾けない。

payload内のvalue/bound/depthが別storeから混在することはない。残る最悪ケースは、
要求keyに別keyのpayloadが対応し、誤ったfail-high/fail-lowや詰みスコアを返すこと。その結果は親局面へ伝播し、探索木、
PV、最善手や棋力に影響し得る。合法なTT手でも発生し、手がNONEでもcutoffし得る。
通常設定のeager NNUEもTT valueによるcutoffまでは無効化しない。

確率は次のように分ける必要がある。

- 独立一様な無関係の短縮キーを仮定すると、1slotの偶然一致は
  `1/65536 ≈ 0.001526%`、3slotの和の上界は `3/65536 ≈ 0.004578%`。
  これは通常の短縮キー衝突の見積りであり、全probeに対する誤cutoff率ではない。
- 同一keyへの並行更新ではkey照合の通過率は100%。異keyへの置換中でも、
  要求したkeyと別書込みのdataが見えれば通過する。ここに一律 `1/65536` は掛けられない。
- probeが不整合snapshotを読む確率を `q`、その条件下でkey/手/範囲/depth/boundの
  検査を通って誤cutoffする確率を `r` とすれば、その寄与は `q × r`。
  実探索のq/rは未計測であり、数値の上限として1より有用な保証は得ていない。
  並行writerが混合状態を残すと、書込み終了後のprobeでも影響を受け得る。
  書込み時間窓だけからqを推定することもできない。

SoAでもkeyとpayloadは同一32バイトcluster内にあり、64バイトcache lineを跨がない。
key→payloadのload/store順は維持するが、Relaxedは両者を同時公開しない。
アクセス回数と配置が変わるため混在する時間窓・頻度は変わり得る。
payload内の混在可能性は減る一方、key/payload混在の実測頻度が減るとは断定しない。
異keyのwriterが交錯すれば混合状態が残る点も同じである。
write時に混在snapshotで置換判定する可能性も残り、単一payload化は探索結果の整合性証明ではない。

したがって小さい通常キー衝突率やTSan通過を、torn entryの無害性・棋力の証拠には使わない。
無害性まで必須ならこの方式だけでは満たせない。全手の合法性検査をprobeへ移しても
value/boundの対応は証明できず、別途一貫性検証またはTT値の利用制限が必要になる。

## SoA実装の検証

- `cargo fmt` / `cargo clippy --workspace --all-targets -- -D warnings`: 通過。
- `cargo clippy --fix --allow-dirty --tests`: sandbox内のTCP listener bindがEPERM。
  自動修正は実行できなかったが、上記の通常Clippyは警告ゼロ。
- `cargo test`: CSA clientの `csa_entering_king_rule` 5件がTCP bindのEPERMで失敗。
  workspace全体のgateは未達で、TCP bindが許可された環境で再実行が必要。
- `cargo test -p rshogi-core`: unit 1,051件、通常doctest 40件、
  drop/resizeのcompile-fail doctest 2件が通過（計16件ignored）。
- `tt-trace,search-stats` 有効のcore全対象ClippyとTTテスト20件: 通過。
- build-std付きThreadSanitizer: TTテスト20件通過。コマンドは後掲と同じ。
- `bash scripts/check-tracked-abs-paths.sh` / `git diff --check`: 通過。

payloadの符号・ビット境界のroundtrip、同一slotの2writer/2readerによるpayload内の
対応、keyだけ異なる混合状態でのprobe/cutoffの限界を検証した。
容量・置換・世代・stale writerの再評価・clear/resizeのテストも維持した。

### 決定性と性能計測の状態

比較用baseは `git archive 0d4c3b64` を一時領域へ展開し、candidateとともに
`cargo xtask build --edition edition-layerstacks-halfka_hm_merged-1536x16x32-none`
でビルドした。worktreeの追加はしていない。

LS 1536x16x32の指定net、kingrank9、1T、Hash 16MBで、次の4局面を
それぞれ独立processの `go depth 8` で比較した。
全局面のdepth 1〜8でnodes、score、PV、最終bestmoveがbaseと一致した。
最終nodesは順に1,167,105 / 66,251 / 26,862 / 119,634。
これは決定性の確認であり、速度の測定結果ではない。

```text
startpos
startpos moves 7g7f 3c3d 2g2f 8c8d 2f2e 8d8e 2e2d 2c2d
sfen 8l/1l+R2P3/p2pBG1pp/kps1p4/Nn1P2G2/P1P1P2PP/1PS6/1KSG3+r1/LN2+p3L w Sbgn3p 124
startpos moves 7g7f 8c8d 2g2f 8d8e 2f2e 4a3b
```

### search_only_ab によるbase比（1T）

同一機で他プロセスのビルドが走ると測定が汚染されるため、load averageが2を下回るまで
待ってから実行した。4局面、movetime 3000ms、abba、rounds 3（各24 run）、CPU pin。

| 実装 | NPS | cycles/node | instructions/node |
|---|---|---|---|
| ロックあり | -0.99% 〜 -1.38% | +0.78% 〜 +0.85% | +0.54% 〜 +0.60% |
| 5-word lockless | -0.55% | +0.46% | +0.46% |
| SoA lockless | **-0.17%** | **+0.26%** | **+0.32%** |

SoA計測時のbaseline cycles/nodeは5828.5で、別セッションで取得した5825.0 / 5830.0と
0.06%以内に一致する。対照条件が動いていないことの確認として記録する。
ロックあり実装の-1.38%側はbaseline cycles/nodeが6002.1で、同一条件の他runより約3%高い。
測定中に負荷が上がったとみられ、-0.99%側より信頼度が低い。

atomicアクセスを1エントリあたり5回から2回へ削減したSoAで、instructions/nodeは
+0.46%から+0.32%へ下がった。NPSはbaseとの差が測定精度に埋もれる水準になった。

静かな環境で、上記4行をpositionsファイルとして次を実行する。
過去の4局面セットの内容は今回提示されていないため、過去の比率との直接比較には
その局面セットを揃える必要がある。

```sh
uptime
./target/release/search_only_ab \
  --baseline /path/to/base/engines/rshogi-usi-layerstacks-halfka_hm_merged-1536x16x32-none \
  --candidate ./engines/rshogi-usi-layerstacks-halfka_hm_merged-1536x16x32-none \
  --positions /path/to/positions.txt --movetime-ms 3000 --pattern abba --rounds 2 \
  --threads 1 --hash-mb 16 --cpu 2 \
  --eval-file "$SHOGI_DATA/nnue/20260713-wrm-n2s1200-1536x16x32/ls-1536x16x32-halfka-hm-merged-wrm-n2s1200-400.bin" \
  --usi-option LS_BUCKET_MODE=kingrank9
```

検証ログ・決定性比較JSON/スクリプトはローカル一時領域の `tt-soa-*` に保存した。

## 5-word lockless実装の既存検証（SoA変更前）

- `cargo fmt` と `git diff --check`: 通過。
- `cargo clippy --workspace --all-targets -- -D warnings`: 警告ゼロで通過。
- `cargo clippy --fix --allow-dirty --tests`: 内部ロック用TCP listenerのbindが
  sandboxの `Operation not permitted (os error 1)` で失敗。通常Clippyで警告ゼロを確認した。
- `cargo test`: CSA clientの `csa_entering_king_rule` の5テストがTCP bindのEPERMで失敗。
  続けて `cargo test --workspace --no-fail-fast` で全対象を実行し、CSA client/server TCP関連の
  8ターゲット・97テストが同じEPERMで失敗した。他の対象は通過したが、全workspace通過とは扱わない。
- `cargo test -p rshogi-core`: 通過。drop/resizeを拒否するcompile-fail doctest 2件も通過。
- `tt-trace,search-stats` を有効にしたcore全対象ClippyとTTテスト: 通過。
- `bash scripts/check-tracked-abs-paths.sh`: 通過。
- ThreadSanitizer: build-std付きでTT関連19テスト通過。TSanの対象はTTテストであり実対局全体ではない。

テストは容量、通常probe/write、置換・世代・clear/resize、stale writer再評価を維持。
ロック競合時のskipテストは撤去。同じslotへ固定した2writerと2readerの負荷で、
全writeの完了とword単位の値域を検証する。word間の整合性を要求するテストは置かない。
別に混合wordを明示的に作るテストと、key不一致・移動元不整合の除外、
probeを通る疑似非合法手とvalue/boundのcutoff判定の限界を固定するテストを追加した。

```sh
cargo test -p rshogi-core
cargo clippy -p rshogi-core --all-targets --features tt-trace,search-stats -- -D warnings
cargo test -p rshogi-core --lib --features tt-trace,search-stats tt::
# 未導入環境では rustup component add rust-src --toolchain nightly
RUSTFLAGS=-Zsanitizer=thread CARGO_TARGET_DIR=target-tsan \
  cargo +nightly test -Zbuild-std --target x86_64-unknown-linux-gnu \
  -p rshogi-core --lib tt::
```

Sanitizerは既存のrustc 1.95.0-nightly (c04308580 2026-02-18) / rust-srcを使用。通常buildはrustc 1.95.0
(59807616e 2026-04-14)。検証ログはローカル一時領域の `tt-unlocked-*.log` に保存した。

## 5-word lockless実装の既存固定時間ABBA比較

以下は過去のmovetime方式の記録で、SoAの性能値ではない。外部build/test負荷の
混入が疑われ、特に2Tの0.9017は静かな環境で再現できなかったと報告されている。
0.5%程度の差の判定には使用せず、探索区間の `search_only_ab` を用いる。

baselineは `0d4c3b64` のgit archiveから新規buildし、本実装も同じ設定でbuildした。
AMD Ryzen 9 5950X / Linux x86_64 / rustc 1.95.0。
release、LTO off、codegen-units 16、target-cpu=native、その他はCargo既定値。

```sh
CARGO_PROFILE_RELEASE_LTO=off CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16 \
  cargo build --release -p rshogi-usi
```

各探索は独立processで `usi` → `usiok` → MaterialLevel=9 / USI_Hash=16 /
Threads=1または2 → `isready` → `readyok` → `position` → `go movetime 1000`
→ `bestmove` → `quit` の順。最終infoのnodesを集計する。
1Tは `taskset -c 4`、2Tは `taskset -c 4,5`（別物理core）に固定。
3局面それぞれでbase→patch→patch→baseを5反復、各Threads・各実装30探索。
反復→Threads (1,2)→局面→ABBAの順で実行し、計測中はこの作業のbuild/testを走らせない。
周波数固定・CPU専有はしていない。

使用局面（USI position引数）:

```text
startpos
startpos moves 7g7f 3c3d 2g2f 8c8d 2f2e 8d8e 2e2d 2c2d
sfen 8l/1l+R2P3/p2pBG1pp/kps1p4/Nn1P2G2/P1P1P2PP/1PS6/1KSG3+r1/LN2+p3L w Sbgn3p 124
```

| Threads | base nodes（30探索） | patch nodes（30探索） | patch/base | bootstrap 95%区間 |
|---|---:|---:|---:|---:|
| 1 | 19,211,548 | 18,527,677 | 0.9644 | 0.9602–0.9689 |
| 2 | 36,621,716 | 33,020,337 | 0.9017 | 0.8318–0.9741 |

**この旧計測だけではbase比の性能劣化を判定できない。** 1Tは約3.56%、2Tは約9.83%のnodes減を観測した。
2Tのブロック比は0.620–1.332と変動が大きい。ロック撤去だけで速度問題が解消したとは言えず、
残るatomic格納・snapshot処理のコストと外部負荷・探索木の影響の切り分けは未実施。

区間は各局面の5個のABBAブロックを復元抽出する層別bootstrap（10,000回、
Python Random seed=1044、合計nodesの比、昇順の250番目/9,750番目）による記述的な推定。
時系列の独立性や正規性を実証した正式な採否検定ではない。

再集計用のABBAブロック合計（各セルはbase / patchの各2探索合計）:

| 反復 | 局面（上記順） | 1T base / patch | 2T base / patch |
|---|---|---:|---:|
| 1 | 1 | 1360232 / 1313864 | 2404432 / 2311925 |
| 1 | 2 | 1196929 / 1174614 | 2288856 / 2183366 |
| 1 | 3 | 1257269 / 1208023 | 2492957 / 2174789 |
| 2 | 1 | 1264318 / 1227679 | 2488575 / 2416778 |
| 2 | 2 | 1102410 / 1061935 | 2445899 / 2335466 |
| 2 | 3 | 1268399 / 1224696 | 2598453 / 2367845 |
| 3 | 1 | 1371842 / 1301260 | 1776728 / 2367247 |
| 3 | 2 | 1233776 / 1187779 | 2426285 / 2318341 |
| 3 | 3 | 1335759 / 1283404 | 2552610 / 2562624 |
| 4 | 1 | 1366563 / 1308128 | 2505191 / 1597572 |
| 4 | 2 | 1207521 / 1191520 | 2422822 / 2348452 |
| 4 | 3 | 1344338 / 1278989 | 2639688 / 2290548 |
| 5 | 1 | 1369076 / 1306735 | 2451495 / 1519248 |
| 5 | 2 | 1227173 / 1185262 | 2441346 / 1810030 |
| 5 | 3 | 1305943 / 1273789 | 2686379 / 2416106 |

別に各実装1回、1Tの `go depth 5` を同じ3局面で実行。nodesは2009 / 546 / 6189、
score/PV/bestmoveも一致した。速度や棋力の証拠には使わない。
全processは正常終了。個別nodes・最終info・wall timeを `tt-unlocked-bench.json`、
実行スクリプトを `tt-unlocked-bench.py`、標準エラーを `tt-unlocked-bench-*.stderr` として
ローカル一時領域へ保存した。base/patchの依存Cargo.lockも一致を確認した。

この比較は短いMaterial評価・固定時間の探索量を対象とする。特に2Tのnodesは
探索木の変化も含み、命令実行コストのみを分離できない。本番NNUE、長時間、多数スレッド、
棋力は未検証。小標本の信頼区間はCPU負荷・周波数変動・局面選択バイアスを保証しない。
棋力の採否判定やSPRTは実施していない。

## ロックあり実装の既存記録

以下は撤去したロックあり実装の記録であり、本実装の測定値ではない。
同じbase、MaterialLevel=9、Hash=16、3局面、movetime 1000、ABBA各1回、CPU固定なし。
当時はrustc 1.97.1、LTO off / codegen-units 16 / target-cpu=native。

| Threads | base nodes（6探索） | ロックあり nodes（6探索） | 比 |
|---|---:|---:|---:|
| 1 | 3,864,926 | 3,747,142 | 0.9695 |
| 2 | 7,374,983 | 7,079,358 | 0.9599 |

同一clusterの2keyに2writer + 2reader各10万probeを集中させた旧harnessでは、
400,000 probe中367,345（91.84%）が排他競合でmissし、writer側probe missは184,640。
probe成功後のwrite競合は未集計だった。この極端負荷は実探索の競合率を示さない。
当時の1T depth 5の3局面はnodes 2009 / 546 / 6189、score/PV/bestmoveがbaseと一致した。
