# rshogi tools

将棋エンジンの対局評価、教師データの生成・加工、性能計測、定跡整備のツール群です。

| やりたいこと | 入口 |
|---|---|
| 教師データを作る・加工する | [gensfen](docs/gensfen.md) / [データ処理の選び方](docs/pack_tools.md) |
| エンジンを対局で比較する | [tournament](docs/tournament.md) → [analyze_selfplay](docs/analyze_selfplay.md) |
| 探索速度を測る | [benchmark](docs/benchmark.md) / [search_only_ab](docs/search_only_ab.md) |
| 探索定数を SPSA で調整する (複数の機械の worker に対局を任せる `--remote-worker`、run-dir の写しの照合 `--verify-resume` も) | [spsa](docs/spsa.md) / [運用手順](docs/spsa_runbook.md) |
| 自己対局の定跡出口と、勝たなかった側が途中で有利になった局面 (`--chance-cp`) を件数と少数の ID 見本で集計し、木のキューの行と既存の木への不足手順の更新行を作る | [book_mine tree seeds](docs/book_mine.md#tree-seeds-自己対局から次に読む根を作る) |
| 台帳を共有して木を並列に読む (すべての読み条件を元の設定のまま共有し、木ごとに `opts.multipv` / `opts.nodes` と `require_condition` を指定可・実行中の台帳の一括追記（検査のメモリ上限つき、不正行と後続の値の衝突を集計して除外し、既存の同値行にも先読みの印を引き継ぐ）・計算量に上限のある任意の先読み・読みかけの局面の途中の上位の手で子を先に読む `--speculate-at`・その先読みを優先度の高い木で先に配る `--speculate-priority`・停止統計つき) | [book_mine tree serve](docs/book_mine.md#空いた-worker-での先読み) |
| 別の台帳の読みを取り込む (複数の台帳を 1 回で、`--offsets` で前回の続きから、`--exclude-readings` で指定した読みを除いて) | [book_mine tree import-journal](docs/book_mine.md#別の台帳の読みの取り込み) |
| 複数台の残り・締めまでの読める量と未着手の木の価値・重複を点検し、未完了の木の再採点 (`--rerank-table` / `--rerank-out`、受付行の `group` の木を最小の priority にそろえる、`--add` の新しい木も同じ物差しで採点して `<prefix>_rerank-add.jsonl` に出す、対象の own ごとの walk 表が必須、元データは段ごとに最大 16 回逐次読み、更新行は hold・計画の出力と併用不可、`--rerank-tiers` で id・区分・今の priority の段の表を当てて priority と group を決める、名前を変えて移した写し (同じ根・同じ own) のある木の hold は外さない)・group ごとに締めまでに読み切る見込みの表 (`--group-table`)・hold 候補と移動・新規追加・手順足し込みの計画 (`<prefix>_plan.tsv` と台別 JSONL、台に入るかは priority の帯の量で判定、移し先は台全体の比 `--move-max-ratio` と帯の読み終わり時間 `--move-band-factor` (既定で有効) で上限を付け、進みの記録の無い台は受付の木を未着手として数える、帯ごとの読み終わり時間 `<prefix>_bands.tsv`、急ぎの帯の偏りを均す `--balance-priority`/`--balance-hours`) を出す (移動元の台へ送った足し込みを木と一緒に移す。重みが確定する own の止まりだけを reach-low にする) | [book_mine tree queue-review](docs/book_mine.md#tree-queue-review-台ごとの残りと未着手の木を点検する) |
| 外部の定跡の木を借りて自分の読みで値を付ける (main は元データ全体で伝播し直した値で選び、nodes も合流後の最善手に揃える) | [book_mine borrow](docs/book_mine.md#borrow-大きい定跡の木を借りる) |
| 木の読みを台帳と照合し、合流を含む値の由来を付けて元データに登録する (serve の稼働中も可、局面を除外する `--skip-keys <FILE>`、反転キーの合流で止まる木ごとに独立に除く局面を探し (`--auto-skip-flip-jobs <N>`、既定 4)、残った問題を木の組と局面の ddmin で探して足す `--auto-skip-flip <OUT>`、curated の局面の main も替える `--replace-curated`、成れる角・飛・歩の不成は main・alt から record にし木の手からも外す、`--single-enqueue-out` の行に `--walk-table` の辿りの重み・下限 `--single-min-weight`・重みの帯ごとの priority `--single-priority-band`) | [book_mine tree register](docs/book_mine.md#定跡の元データへの登録) |
| 読み終わった木の出力先を根の下から集め、登録済み・未完了・id の接頭辞・段 0 だけ・外した読みそのもの (key・nodes・sec が全部一致) を含む木・登録済みの木や先の候補と内容が同じ写しを判定つきの表に出し、残りを `tree register` に渡す一覧にする | [book_mine tree inventory](docs/book_mine.md#tree-inventory-登録していない読み終わった木の一覧) |
| 複数のserveの進み・1 engine と全 worker の NPS・NOT_READYの終了件数・世代別の深さ・先後別に重複を除いたDONE実績による残り・idleまでの時間を表示する | [book_mine tree status](docs/book_mine.md#tree-status-複数の台の進みと-idle-までの時間) |
| 定跡の 13 列の元データ・読み条件・値の由来・schema の語彙・合法手と、main・alt に成れる角・飛・歩の不成が無いことを検査する | [book_mine master validate](docs/book_mine.md#master-validate-元データの全件検査) |
| 元データに行を追加・置換する（curated の全列置換も対象） | [book_mine master add](docs/book_mine.md#master-add-元データへの行の追加と置換) |
| 序盤の main の替わりを継続手数で hold / replace に分け、指定キーを優先して保留する (`--continuation`・`--hold-keys`・`--deep-opp-freq`・`--deep-min-games`)。hold の継続が足りない枝の途切れた局面の一覧 (`--cuts-out`) と、そこを根にした木のキューの行 (`--cuts-enqueue-out`、既存キューによる除外 `--cuts-exclude-queue`) | [book_mine master hold-early](docs/book_mine.md#master-hold-early-序盤の-main-の替わりを保留に戻す) |
| 読みのログから整数評価値の追加候補と報告を作る | [book_mine master add-reads](docs/book_mine.md#master-add-reads-読みから追加候補を作る) |
| 元データから定跡・プリセット・条件と探索深さの注釈を書き出す（tree 台帳以外と不正な読み・後続の値の衝突は照合から除外し、失敗時は既存出力を復元） | [book_mine master export](docs/book_mine.md#master-export-定跡プリセット注釈の書き出し) |
| 注目の線から元データと DONE の木を合わせ、伝播値つきの定跡をビュアー用に作る | [book_mine master line-db](docs/book_mine.md#master-line-db-注目の線を辿る定跡) |
| 2 つの定跡の局面と手の差分を表示する | [book_mine master diff](docs/book_mine.md#master-diff-2-つの定跡の差分) |
| 書き出した定跡とプリセットを検査する | [book_mine master check](docs/book_mine.md#master-check-書き出した定跡とプリセットの検査) |
| 伝播の切れ目の局面を重み順に並べ、既存の木との重複を除いたキューにする `--bridge-enqueue-out` と、main の置換と、旧 main・curated を保護した悪い alt の bench（控え）への降格・値が `--bad-eval` より大きく基準との差が `--demote-line` (既定 100) 未満の控えの復帰（`--apply`・`--replace-curated`・`--demote-alts`・`--revive-bench`。降格・復帰は `--demote-line 100`、点検・main 置換は `--line 40`。`--opp-records` で相手の record も伝播に使い、反転した局面へは辺を張らない。成れる角・飛・歩の不成は main・alt から record にし、控えからも戻さない。`--single-enqueue-out` の行に `--walk-table` の辿りの重み・下限 `--single-min-weight`・重みの帯ごとの priority `--single-priority-band`） | [book_mine master audit](docs/book_mine.md#master-audit-定跡の元データの点検) |
| 自分は main、相手は強い対局の頻度か大きい定跡の手で指すとして辿り、自分が読み始める局面と重みを出す (`--opp-records` で強い対局が止まりの局面とその 10・20 手前に着いた局数・得点・勝敗の列) | [book_mine master walk](docs/book_mine.md#master-walk-自分が読み始める局面と重み) |
| 初期局面から先手と後手の main を交互に直接キーで辿り、定跡が切れる局面 (局面なし・反転キーでだけある・main なし・循環) と理由、そこを根にした own=bw の木の行を出す | [book_mine master main-line](docs/book_mine.md#master-main-line-main-対-main-で定跡が切れる所) |
| walk の止まりと伝播の切れ目・定跡に戻る手の表を合わせ、出口を有望・塞ぎたい・浅い・値の出所が弱いに分けた表と Markdown の報告を出す (受付の木の照合つき。局面の番号は全部の節で「何手目」= その局面で次に指す手の番号、先手番は奇数・後手番は偶数。表は局面の手番 `turn` と `move_number` の列を持ち、偶奇が合わなければエラー) | [book_mine master exit-report](docs/book_mine.md#master-exit-report-版ごとの出口の報告) |
| 戦型の候補ごとに、RAMU の手番の決めた局面で main の代わりに決めた手を指して辿り、出口の値・確度・強い対局の実績・危ない局面を通る重み・分岐の後の相手の手を比べる表と Markdown の報告を出す (報告の局面の番号は「何手目」= その局面で次に指す手の番号) | [book_mine master opening-report](docs/book_mine.md#master-opening-report-戦型の候補を比べる表) |
| walk と同じ辿りで RAMU の手番の局面の main の伝播の値と外部定跡の値を比べ、食い違う局面の表と Markdown の報告、定跡が切れた所を根にした木のキューの行を出す (受付の木の照合つき) | [book_mine master opp-book-diff](docs/book_mine.md#master-opp-book-diff-外部定跡と食い違う局面) |
| 自分は main、相手は main・alt を指して辿れる出口を層ごとの外部ソート・到達済みキーのマージで分類し、既存の木との重複を除いてキューにする | [book_mine master exits](docs/book_mine.md#master-exits-元データを辿って出口を列挙する) |
| LayerStacks の FT と L1 の列を並べ替える | [permute_nnue_ft](docs/permute_nnue_ft.md) |
| その他のツールを探す | [目的別の全ツール一覧](docs/tools-reference.md) |

`master audit --deep-plies N`（N は 1 以上）は、両方の手の先で元データの main が N 手以上続けば線未満も対象にします。直接のキーが無いときは反転キーでも続きを辿ります。
`master audit` と `tree register` の `--switch-rule propagated` は線を使わず、main と 1 位の値がどちらも手の先の局面から来ていれば差が 1 でも main を替え、片方が 1 回の読みなら替えずに、その手を伸ばす木の行を `--single-enqueue-out` に出します (`--exclude-queue` で既に積んだ木を除く)。
`master audit` と `master rejoin` の重み (`--opp-freq`) は `--opp-book <PATH>` で、局数が足りない局面と局数の少ない手に行く分を大きい定跡の手で辿ります (`master walk --opp-book` と同じ割り振り)。
`master audit` と `master hold-early` は `--deep-opp-freq <PATH>` で頻度定跡を渡すと、相手の main に加えて `--deep-min-games <N>` (既定 3) 局以上の応手をすべて辿り、最短の継続手数で判定します。頻度定跡は読み込み時に下限未満の手を除外します。audit では `--deep-plies` が必要で、`--deep-min-games` の明示指定には頻度定跡が必要です。

控え（bench）の復帰は `tree register`（控え自身の source 保護を適用）と `master audit --revive-bench` で行い、`borrow value` では復帰させません。書き出しでは record と同じく DB に -30000 で残し、プリセットから除きます。

## ビルドと実行

リポジトリのルートで実行します。必要な Rust 環境は [ビルド手順](../../docs/build.md) を参照してください。
以下は Bash / Git Bash で実行する例です。Windows native のエンジンの実行ファイルには `.exe` を付けます。

```bash
cargo build --release -p tools --bin shuffle_psv
cargo run --release -p tools --bin shuffle_psv -- --help
```

通常は必要なバイナリだけを `--bin` で選びます。`kifu_player` と `dlshogi-onnx` は
既定で有効です。TUI・ONNX 依存を外す場合は
`--no-default-features --features nnue-arch` をビルド引数に追加します。
`prepack_nnue` は追加の `--features prepacked-nnue` が必要です。
1 つの NNUE 構成だけを入れた速い build (`rescore_psv` 等) は
[rescore_psv の単一構成の build](docs/rescore_psv.md#単一構成の-build) を参照してください。
静的 LayerStacks を直接読む診断ツール (`bench_nnue_eval` / `eval_sfens` / `nnue_saturation` /
`verify_nnue_accumulator`) には、tools の `layerstack-arch` feature (`nnue-arch` に含まれる) が必要です。

## クイックスタート

教師データ・NNUE モデル・進行度係数は repo 外の共有 root に置き、
環境変数 `SHOGI_DATA` に設定してください。以下の入力ファイルはあらかじめ用意します。
出力は実行ごとに別名にすると前回の結果を保持できます。

### 教師データの生成とシャッフル

```bash
# model.bin は使用する評価関数に置き換える。
RUN="$SHOGI_DATA/teachers/generated-$(date +%Y%m%d_%H%M%S)"
cargo run --release -p tools --bin gensfen -- \
  --eval-file "$SHOGI_DATA/nnue/model.bin" \
  --games 100 --nodes 80000 --threads 1 --hash-mb 256 \
  --out-dir "$RUN"

cargo run --release -p tools --bin shuffle_psv -- \
  --input "$RUN/gensfen.psv" --output "$RUN/shuffled.psv" \
  --seed 42 --chunk-size 1000000
```

`gensfen` は終了時に局面数を表示し、指定先へ PSV と結果 JSONL を保存します。
LayerStacks の routing・進行度係数が必要なモデルは
[起動時の設定条件](docs/nnue-routing.md#起動時の設定条件)に従って指定してください。
`shuffle_psv` のチャンク指定は全件のメモリ保持を避けるためです。メモリ・一時ディスク容量と
中断時の扱いは [shuffle_psv](docs/shuffle_psv.md)、生成の drain・再開は
[gensfen](docs/gensfen.md) を参照してください。学習自体は外部トレーナーで行います。
[bullet-shogi](https://github.com/SH11235/bullet-shogi/tree/shogi-support) を使う場合の学習手順は、そちらの文書を参照してください。

### 2 エンジンの対局と集計

評価関数などの初期設定が済んだ USI エンジンを用意します。
同じバイナリでモデルを比較する例は [tournament](docs/tournament.md) を参照してください。

```bash
RUN="runs/selfplay/$(date +%Y%m%d_%H%M%S)-compare"
cargo run --release -p tools --bin tournament -- \
  --engine /path/to/candidate --engine-label candidate \
  --engine /path/to/base --engine-label base \
  --games 10 --byoyomi 1000 --threads 1 --hash-mb 256 \
  --concurrency 2 --seed 42 --out-dir "$RUN"

cargo run --release -p tools --bin analyze_selfplay -- "$RUN"
```

`--games 10` は各方向 10 局なので、この例は先後を交換して計 20 局です。
開始局面集を使う比較では `--startpos-file` を追加します。
Fischer 時計の NPS による補正（`--nps-scaling`）と Linux の NUMA 固定（`--cpu-affinity numa`）は
[tournament の時計補正と CPU 割り当て](docs/tournament.md#機械の速さによる時計の補正---nps-scaling)を参照してください。
中断・動的な局数変更・SPRT は [tournament](docs/tournament.md)、
不完全入力の扱いは [analyze_selfplay](docs/analyze_selfplay.md) を参照してください。

### 探索の動作確認

```bash
cargo run --release -p tools --bin benchmark -- \
  --internal --usi-option MaterialLevel=1 \
  --limit-type nodes --limit 10000 --tt-mb 16 --eval-hash-mb 16
```

駒得評価で短く探索し、コンソールに NPS、`benchmark_results/` に JSON を保存します。
NNUE を使った性能比較では `--nnue-file` と必要な routing を明示し、
[benchmark](docs/benchmark.md) または [search_only_ab](docs/search_only_ab.md) の条件を揃えて計測します。

## 共通の注意と関連資料

- 各 CLI の全オプションは `cargo run --release -p tools --bin <名前> -- --help` で確認できます。
- 共通の入力ファイル収集は、読めないディレクトリを黙って除外せずエラーを返します。
- 入出力の保護・上書き・再開の契約はツールごとに異なります。各 doc の出力に関する説明を確認してください。
- [prep_hcpe](docs/prep_hcpe.md) はメモリ予算に応じて shuffle 方式を切り替え、開始時に使用量の見積もりを表示します。
- [HCPE の 38 byte 形式と padding](docs/hcpe-format.md)、[LayerStacks routing と進行度係数](docs/nnue-routing.md)。
- CSA client のログは単局集計です。未決の対局は勝敗集計・教師抽出から除外し、SPRT の対象にしません。
  詳細は [解析](docs/analyze_selfplay.md) と [KIF 変換](docs/jsonl_to_kif.md) を参照してください。
- Floodgate の HTTPS 取得は OS の証明書ストアを使います。最小 Linux コンテナには `ca-certificates` を用意してください。
  暗号 provider は `ring` で、ビルドには C コンパイラが必要です（CMake / NASM は不要）。
- [コマンド例集](examples/README.md)、[教師データの追加パイプライン](docs/training_data_pipeline.md)。
- CSA 接続クライアントは別 crate です。[csa_client の手順](../rshogi-csa-client/examples/README.md)を参照してください。
