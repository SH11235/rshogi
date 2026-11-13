# rshogi tools リファレンス

[クイックスタート](../README.md)から始め、用途に合うツールの詳細を開いてください。
一覧は `src/bin/` の全バイナリを含みます。個別 doc がない補助ツールはソースへリンクしています。
全オプションは `cargo run --release -p tools --bin <名前> -- --help` で確認できます。
`book_mine master rejoin` の応手集約とメモリの扱いは[詳細](book_mine.md#master-rejoin-相手の手で定跡に戻ると値が下がる局面)を参照してください。

## 教師データの生成・変換・加工

| ツール | 用途 |
|---|---|
| [`gensfen`](gensfen.md) | エンジン対局から PSV / pack / hcpe3 教師を生成 |
| [`nyugyoku_gensfen`](nyugyoku_gensfen.md) | CSA から入玉局面を抽出し gensfen 用開始局面を生成 |
| [`policy_rollout_psv`](policy_rollout_psv.md) | ONNX policy で局面を進め、value 評価付き PSV を生成 |
| [`shuffle_psv`](shuffle_psv.md) | seed とチャンクサイズを指定して PSV をシャッフル |
| [`split_psv`](split_psv.md) | PSV を件数・容量で分割し、入力を出力 alias から保護 |
| [`merge_psv`](merge_psv.md) | 複数 PSV を指定順にストリーミング結合 |
| [`prep_hcpe`](prep_hcpe.md) | hcpe の汚染・重複除去、予算内のメモリ内 shuffle / ディスク切替、メモリ見積もり、件数制限・分割 |
| [`preprocess_psv`](preprocess_psv.md) | qsearch leaf への置換と、変更行の bitmap mask 出力 |
| [`filter_teacher_data`](filter_teacher_data.md) | 王手・評価値による抽出、クリップ、統計出力 |
| [`psv_profile`](psv_profile.md) | 手数・入玉・宣言・勝敗の分布と、棋譜順 PSV の対局ごとの終局を集計 |
| [`validate_psv`](validate_psv.md) | PSV の不正局面を検出・除去し、入力 alias を拒否 |
| [`validate_sfens`](../src/bin/validate_sfens.rs) | SFEN テキストの不正局面を検出・除去 |
| [`relabel_psv`](relabel_psv.md) | 勝敗による score 置換、宣言勝ち・diversion の検査 |
| [`rescore_psv`](rescore_psv.md) | NNUE / 探索 / 外部 USI / ONNX で再評価し、再開・score sidecar に対応。探索はスレッド数によらず同じ出力。ONNX は 1 回の実行に渡した全ファイルでセッションを使い回し、`--onnx-gpu-id 0,1` で複数 GPU を使える |
| [`rescore_hcpe`](rescore_hcpe.md) | hcpe の eval を固定 depth の NNUE 探索で再評価・再開 |
| [`expand_psv_from_policy`](expand_psv_from_policy.md) | ONNX policy で PSV の候補局面を展開 |
| [`psv_gate_by_king_zone`](psv_gate_by_king_zone.md) | 入玉条件による score 合成と行対応 mask の生成 |
| [`psv_dual_label`](psv_dual_label.md) | 通常・dual-label PSV と score / mask sidecar の相互変換 |
| [`psv_entered`](psv_entered.md) | 入玉の行の抜き出しと、付け直した入玉の行からの dual-label PSV の作成 |
| [`psv_select_by_mask`](psv_select_by_mask.md) | mask の bit 1 に対応する PSV 行を入力順に抽出 |
| [`psv_scatter_by_mask`](psv_scatter_by_mask.md) | compact PSV の score を元 shard の mask 対応行へ反映 |
| [`fix_scores`](fix_scores.md) | 元 PSV を保護して前処理済み PSV の score を復元 |
| [`psv_to_jsonl`](psv_to_jsonl.md) | PSV を確認用 JSONL に変換 |
| [`jsonl_to_psv`](jsonl_to_psv.md) | 自己対局 JSONL から PSV を生成し、破損行を件数付きで除外 |
| [`jsonl_to_kif`](jsonl_to_kif.md) | JSONL から先後・終局理由を反映した KIF を局単位で生成 |
| [`psv_to_hcpe3`](psv_to_hcpe3.md) | PSV を cshogi 互換の hcpe3 / hcpe へストリーミング変換 |
| [`hcpe_to_psv`](hcpe_to_psv.md) | 38 byte hcpe を 40 byte PSV に変換 |
| [`pack_to_psv`](pack_to_psv.md) | GenSfen pack を有界バッチで PSV に変換 |
| [`hcpe3_to_psv`](hcpe3_to_psv.md) | dlshogi hcpe3 の棋譜を局面ごとの PSV に展開 |
| [`migrate_psv_move16`](migrate_psv_move16.md) | 旧リポジトリ形式の move16 を YaneuraOu 形式に移行 |
| [`psv_dedup`](psv_dedup.md) | 64 bit hash 集合で重複除去。メモリは件数比例、hash 衝突の可能性あり |
| [`psv_dedup_bloom`](psv_dedup_bloom.md) | 固定サイズ Bloom filter による近似重複除去・差分抽出 |
| [`psv_dedup_partition`](psv_dedup_partition.md) | ディスク分割による exact 重複除去と任意の fused shuffle |
| [`psv_dedup_check`](psv_dedup_check.md) | 重複率を集計。hash 集合または固定サイズ近似テーブルを使用 |

## 対局・棋譜・外部連携

| ツール | 用途 |
|---|---|
| [`tournament`](tournament.md) | 複数エンジンの総当たり・先後交換対局、SPRT (境界到達後も開始済みペアを完結し、停止時の判定を meta.json に保存)、動的制御、開始局面ファイルの動的読み直しと均一巡回、NPS 時計補正・Linux NUMA 固定 |
| [`analyze_selfplay`](analyze_selfplay.md) | 対局 JSONL の勝率・Elo・SPRT・開始局面別成績を集計（result の局面・名前にも対応） |
| [`floodgate_record`](floodgate_record.md) | csa_client の対局ログから先後別・相手別成績と NPS を集計 |
| [`floodgate_pipeline`](floodgate_pipeline.md) | Floodgate 棋譜の取得、128 棋譜ずつの局面抽出、live mirror |
| [`kifu_player`](kifu_player.md) | PSV / JSONL / CSA の再生・検索・追記監視を行う TUI |
| [`kifu_publish`](kifu_publish.md) | JSONL / CSA / KIF とイベント情報を棋譜サーバーへ公開 |

## 性能計測・NNUE 診断・ラベル評価

| ツール | 用途 |
|---|---|
| [`benchmark`](benchmark.md) | 内部 API または外部 USI の探索量・時間・NPS を計測 |
| [`bench_nnue_eval`](bench_nnue_eval.md) | 固定・巡回局面の NNUE eval / refresh を分けて計測 |
| [`search_only_ab`](search_only_ab.md) | 探索だけの A/B 比較と Linux perf / Windows ETW の計測 |
| [`mate_bench`](mate_bench.md) | df-pn 詰み探索の正解・探索量・時間を集計。最短手順 (詰将棋ルール / 全応手) と作意手数との照合、変長駒余りの候補の検出 (`--check-leftover`) |
| [`compare_nodes`](compare_nodes.md) | 2 エンジンの深度別ノード数を比較し、通信終了をエラーとして検出 |
| [`compare_eval_nnue`](compare_eval_nnue.md) | PSV をサンプリングして NNUE の静的評価・探索スコアを比較 |
| [`eval_sfens`](eval_sfens.md) | SFEN 局面集を NNUE で静的評価 |
| [`nnue_saturation`](nnue_saturation.md) | LayerStacks の活性飽和率と bucket 分布を計測 |
| [`verify_nnue_accumulator`](verify_nnue_accumulator.md) | NNUE の差分更新と再計算の一致を検証 |
| [`dump_effect_bucket_golden`](dump_effect_bucket_golden.md) | effect bucket の active index を設定別に dump |
| [`prepack_nnue`](prepack_nnue.md) | LayerStacks FT を事前展開して共有読み込み用コンテナを生成 |
| [`permute_nnue_ft`](permute_nnue_ft.md) | LayerStacks の FT と L1 列を評価値を保って並べ替え |
| [`extract_bench_positions`](extract_bench_positions.md) | CSA / 自己対局 JSONL からラベル品質測定用の局面を抽出 |
| [`label_bench_positions`](label_bench_positions.md) | ベンチ局面へ深い NNUE 探索の評価ラベルを付与 |
| [`label_bench_dl`](label_bench_dl.md) | ベンチ局面へ dlshogi ONNX の静的評価を付与 |
| [`yardstick_label`](yardstick_label.md) | held-out hcpe の保存教師値を保持し、labeler の評価ラベルを付与 |
| [`yardstick_score`](yardstick_score.md) | ラベルの WDL logloss・相関・基準との一致を採点 |
| [`ek_testset`](ek_testset.md) | held-out CSA から入玉評価テストセットを構築・採点 |
| [`nyugyoku_metrics`](nyugyoku_metrics.md) | 入玉・詰みの距離と静的評価の順序一致を採点 |

## 定跡

| ツール | 用途 |
|---|---|
| [`book_from_csa`](book_from_csa.md) | CSA 棋譜から即指し・レート・勝敗で絞ったテキスト定跡を生成 (`--book-of` で片方の手番の定跡が続く間の両者の手) |
| [`book_kachi_label`](book_kachi_label.md) | CSA の宣言勝ち決着率を定跡の候補手へ付与 |
| [`book_prepwalk`](book_prepwalk.md) | 相手の準備を自分の定跡で辿り、出口と準備の残り手数を集計 |
| [`book_rescore`](book_rescore.md) | 候補手を USI / ONNX で評価し、内容指紋で再開を検査 |
| [`book_extend`](book_extend.md) | USI bestmove を count=0 で候補集合へ追加 |
| [`book_backprop`](book_backprop.md) | 千日手・連続王手を考慮した後退解析で評価値を逆伝播 (反転キーで合流した局面も向きごとに循環の保証を検査) |
| [`book_mine`](book_mine.md) | 混在条件の tree 台帳 (読み込み時に指紋の再計算・同じ指紋の設定一致を検査し、スキップ件数を指紋別に表示、取り込み元の指紋を列挙)・複数台の未完了の木を最新の walk で再採点する `tree queue-review --rerank-table / --rerank-out` (受付行の `group` の木を最小の priority にそろえる、`--add` の新しい木も同じ物差しで採点して `<prefix>_rerank-add.jsonl` に出す、対象の own ごとの walk 表が必須、元データは段ごとに最大 16 回逐次読み、更新行は `--hold-out`・`--plan-out` と併用不可、`--rerank-tiers` で id・区分・今の priority の段の表を当てて priority と group を決める、名前を変えて移した写し (同じ根・同じ own) のある木の hold は外さない)・group ごとに締めまでに読み切る見込みの表 `tree queue-review --group-table`・複数台の残りと未着手の根を分類し、移動・新規追加・手順足し込みの計画 (`<prefix>_plan.tsv` と台別 JSONL) を出す `tree queue-review --plan-out` (台に入るかは priority の帯の量で判定し、移し先は台全体の比 `--move-max-ratio` と帯の読み終わり時間 `--move-band-factor` (既定で有効) で上限を付け、進みの記録の無い台は受付の木を未着手として数え、帯ごとの読み終わり時間 `<prefix>_bands.tsv`、急ぎの帯の偏りを均す `--balance-priority`/`--balance-hours`、重みが確定する own の止まりだけを reach-low にし、重複 ID は最大 8 件と総数)・自己対局の定跡出口から、起点・根ごとに先手全勝を除いて木のキューと既存の木への不足手順の更新行 (`--update-out`、元ファイルと追加本数の TSV は `--update-sources`) を作る `tree seeds` (`--sweep-loser` で先手全勝の起点も全敗した後手側だけを early と同じ優先度で採用) (`--exclude-queue` は受付順。同一 id の `game` は初出順で重複なく統合し、他の欄は最後の行を使用。`tree enqueue --from` は入力内の重複 id を追記前に拒否し、受付済み id の差し替えは許可。根は局数と最大 8 件の ID 見本、手順は局数で集計)・定跡の展開・借用 (借りる定跡は `.db` / `.ybb`。切り出しの読む局面を台の速さの比で重ならないように分ける `borrow cut --parts`・確認点の印を差によらず付ける `borrow value --mark-all`。相手の定跡を先に引き無い局面だけ頻度で指す `master walk --opp-book-first`。木の循環の値を先後別に付ける `tree` の `draw_black` / `draw_white`。自分の読みで値を付けて (部分木の相手の手番の局面の手は指さない record で) `source=ext`・`lineage=book:<ファイル名>@<sha256 の先頭 8 文字>` で元データに登録。登録する値と main・alt は、全部の根の手を元データ全体で `master audit` と同じ規則で伝播し直した値で選ぶ)・木からの登録 (`tree register`: `borrow value` と同じく `tree:K` は行き先の残り段数 + 1、schema 省略時の置換対象 source は `read`・`ext`、`--replace-curated` で curated の局面の main も替え、`--skip-keys <FILE>` で指定局面と反転キーの局面を除外する。`--bench-expanded` で木の中で先を読んだ手を、`--bench-single` で 1 回の読みの手と木の中で先を確かめられない手を、差が `--alt-gap` 以上でも bench で残す。`--auto-skip-flip <OUT>` で登録後に `master cycles` と同じ伝播の反転キーの合流を確かめ、止まるなら最初に木ごとに独立に除く局面を探す (`--auto-skip-flip-jobs <N>`、既定 4、1 以上)。対局で定跡を直接のキーだけで引くなら、`master cycles`・`master rejoin` の `--direct-lookup` で反転キーの合流では止めずに解ける (確かめなかった局面は `--direct-lookup-table` に出す)。その後は木の組み合わせで止まる分を全部の木の局面から並列に絞って (通らないときだけ木の組と局面の ddmin) 除く局面に足し、通るまで (`--auto-skip-flip-rounds`、既定 10 回、木ごとの段は数えない) メモリの上で繰り返して足した局面を OUT に書く。main を替える前に登録後の元データを `master audit` と同じ伝播で解き、元の main が木の 1 位を `--keep-line` (既定 20) 以上上回り、その値が `--keep-min-depth` (既定 1) 局面以上先の読みから来ていれば替えずに食い違いの一覧に `reason=keep` で値・段数・差を出す。`--switch-rule propagated` では両方の値が手の先の局面から来て木の 1 位が上回るときだけ替え、片方が 1 回の読みなら替えずに `reason=keep` に回してその手を伸ばす木の行 `--single-enqueue-out` を出す (行に `--walk-table` の辿りの重み・下限 `--single-min-weight`・重みの帯ごとの priority `--single-priority-band` を付けられる)。合流を含む子からの伝播で値の由来を判定、複数台帳との lock なしの照合 (serve の稼働中も可)、読みの条件が 1 つに決まらない木は `cond` を手ごとに書く (1 回の読みの手は局面の読みの条件、先を読んだ手は行き先から辿れる読みが同じ条件ならその名前・違えば `mixed`)、値が同じなら既知の条件を保持)・登録していない読み終わった木の一覧 (`tree inventory`: 根の下の `registration_status.json` を持つ出力先を集め、登録済みの一覧は実体のパスで照合し、`--excluded-readings` は key・nodes・sec が全部一致する読みを持つ木だけを外し (一覧は一定の件数ずつ表に載せ、木の読みは一時ファイルに並べて照合するのでメモリは行数に比例しない)、id・own・世代と内容が同じ写しは `registered-copy`・`duplicate` にして候補から外し、全部の出力先の判定と理由を TSV に出す)・元データの行の追加と置換 (`master add`: curated も全列置換、source 集計は schema 順)・読みからの追加候補と報告 (`master add-reads`: 追加する評価値は整数、メモリは読み・候補・報告の量に比例)・元データからの書き出し (`master export`: DB・2 種のプリセット・注釈、13 列の元データ・schema の語彙検査・version 2 注釈 (schema の `conditions` と `--journal` の条件別探索深さ `depths`、tree 台帳以外のファイルは照合から除外)、読みの集計 (定跡に無い局面の読みは保持しない)、出力失敗時の復元)・注目の線と DONE の木を合わせたビュアー用定跡 (`master line-db`: main・alt と木を再帰的に合成し、replace で伝播)・DB の差分 (`master diff`)・序盤の main の替わりを継続手数で hold / replace に分類して一覧にし、hold だけを戻す (`master hold-early --continuation` (audit と共通の `--deep-opp-freq <PATH>`・`--deep-min-games <N>` (既定 3) で頻出する相手の全応手の最短継続を判定)・`--hold-keys` で指定局面を優先して保留。hold の継続が足りない枝の途切れた局面と理由・手順の一覧 `--cuts-out` と、そこを根にした木のキューの行 `--cuts-enqueue-out` (同じ局面・同じ側は 1 行、`--cuts-exclude-queue` で walk と同じ除外))・元データの全件検査 (`master validate`、main・alt に成れる角・飛・歩の不成があれば誤り)・書き出した定跡とプリセットの検査 (`master check`)・監査 (main・alt の成れる角・飛・歩の不成を record にして main を選び直し (`tree register` も同じ、`borrow value` は外部定跡の手から外す)、main の点検と置換 (main と 1 位は同値の -1 補正を戻した値で比べ、同値なら main を保つ。千日手の値の既定は先手 -70 / 後手 70。`--opp-records` で相手の record も、`--opp-bench` で相手の bench も伝播に使い、反転した局面へは辺を張らない。既定の点検と反駁出力では record・bench の全列を保持せず、局面キーと手は重複検査に使う。重みの辿りに大きい定跡も使う `--opp-book` (rejoin と共通、walk と同じ割り振り)・`master audit --apply --since TAG`・`--schema`・両方の main の継続手数で線未満も対象にする `--deep-plies`: 伸ばした手だけ main に替え、読みの値は保持。線の代わりに値の出どころで決める `--switch-rule propagated` (main と 1 位がどちらも手の先の局面から来れば差が 1 でも替え、片方が 1 回の読みなら替えずにその局面を根にその手を `game` に入れた木の行 `--single-enqueue-out`、`--exclude-queue` はその手を `game` で指す木かその手の先を根にした木で除く、行に `--walk-table` の辿りの重み・下限 `--single-min-weight`・重みの帯ごとの priority `--single-priority-band` を付けられる)。`--replace-curated` で curated の局面も置換、`--demote-alts` (`--demote-line` は既定 100、点検・main 置換の `--line` は既定 40。旧 main・curated を保護し、伝播した手と main の補正前の値を基準に降格) で悪い alt を bench (控え) にし、`--revive-bench` で値が `--bad-eval` より大きく基準との差が `--demote-line` (既定 100) 未満の控えを alt に戻す。控え自身の source 保護を適用し、`borrow value` は復帰させない。bench は record と同じく DB に -30000 で残し、プリセットから除く。元データに無い相手の手番の局面を補う `--fill-replies` と、読みの値のある手の行き先を補って読みの値と定跡の応手の先の値の悪い方を取る `--bridge-replies` (悪くなった main の表 `--bridge-table`・main を伸ばす木のキュー `--bridge-enqueue-out` (既存キューによる除外、到達重みの並べ替え可))。`--fill-replies` は `--apply`・`--demote-alts`・`--revive-bench` と併用不可、`--bridge-replies` は併用でき、補う伝播は反転した局面へ辺を張らない)・反駁・再合流・循環を塞ぐ手。外す手の数が最小の組、外してよい手の制限つき。外してよい手だけで塞げなければ、局面を消さずに循環へ入る手を `--entry-depth` 段まで上へたどって record にし (curated の手を除く、親の main は値の一番高い手に替える)、それでも塞げなければ書き換えずに止まる `master cycles`)・読み始める局面と重みの辿り (`master walk`: 根の手数・既存の木によるキューの絞り込み、読み条件の指定、`--opp-records` で強い対局が止まりの局面とその 10・20 手前に着いた局数・得点・勝敗の列)・main 対 main で定跡が切れる局面と理由・そこを根にした own=bw の木の行 (`master main-line`: 直接キーで辿り、反転キーでだけある局面も切れ目)・版ごとの出口の報告 (`master exit-report`: walk の止まり・bridge・rejoin を有望・塞ぎたい・浅い・値の出所が弱いに分け、受付の木を照合。局面の番号は全部の節で「何手目」= その局面で次に指す手の番号 (先手番は奇数、後手番は偶数) で、表は `ply` の代わりに局面の手番 `turn` と `move_number` の列を持ち、偶奇か手順の手数が合わなければエラー。rejoin の節は walk の表から取った手順と受付つき)・外部定跡と食い違う局面 (`master opp-book-diff`: walk と同じ辿りの RAMU の手番の局面で、main の伝播の値と外部定跡の値の差・定跡が切れる所・外部定跡の続き・受付の照合、切れた所を根にした木のキューの行)・戦型の候補の比較 (`master opening-report`: 候補ごとに RAMU の手番の決めた局面の手を替えて walk と同じく辿り、出口の値・確度・強い対局の実績・危ない局面を通る重み・分岐の後の相手の手の表。報告の局面の番号は `master exit-report` と同じ「何手目」)・元データの出口の分類とキュー化 (`master exits`: 層ごとの外部ソート・到達済みキーのマージで探索と整列のメモリを制限) と、キュー・台帳を使う並列探索 (計算量に上限のある任意の先読みと USI stop による横取り、読みかけの局面の途中の上位の手で子を先に読む `--speculate-at` と、その先読みを優先度の高い木で本来の読みより先に配る `--speculate-priority`。すべての読み条件を元の設定のまま共有し、自条件が無い局面はノード上限・PV 行数・指紋で決定的に選び、再取り込みは同値行を追記しない。`borrow value` も読みごとに条件を保持し、自身の読みのノード上限を優先し、読みの無い途中局面には相手番も経由して合流後の最善手の子の上限を伝播。木ごとに `opts.multipv` / `opts.nodes` と `require_condition` を指定可。稼働中の `tree serve` にも `state/import/` で台帳を取り込める (ディスクのバケットで検査のメモリを制限)。別のマシンの台帳の取り込みは、`--offsets` で差分を再開でき、差分の同じ条件・局面・値の行は先読みの印を引き継いで集約する。宛先と競合する行、または差分内に複数の値を持つ条件・局面があるときだけソースの先行範囲を一度走査して内部の食い違いを除く。値の違う重複を拒否するか、取り込み先の値を残して一覧に出すかを選べる。開発者向けに、台帳の読みだけで serve の配り方を流して worker の空きを見積もる模擬テストあり) |
| [`book_mine tree status`](book_mine.md#tree-status-複数の台の進みと-idle-までの時間) | 複数のserveの進み・1 engine と全 worker の NPS・NOT_READYの終了件数、(id, own) の重複を除いたDONEの実績による残りとidleまでの時間 |

## SPSA・パラメータ

| ツール | 用途 |
|---|---|
| [`spsa`](spsa.md) | 永続エンジンプールで同期 / 非同期 SPSA を実行し、途中再開。非同期では対局を別の機械の worker に任せられる (`--remote-worker`)。run-dir の写しが resume できるかの照合 (`--verify-resume`) |
| [`spsa_param_diff`](spsa_param_diff.md) | SPSA パラメータと既定値・履歴の差を集計 |
| [`generate_spsa_params`](generate_spsa_params.md) | 探索パラメータの既定値から c_end / r_end 形式の params を生成 |
| [`generate_net_spsa_params`](generate_net_spsa_params.md) | LayerStacks の重み delta 用 SPSA params を生成 |
| [`apply_net_spsa_params`](apply_net_spsa_params.md) | 重み delta を NNUE へ反映し、読み戻しと SHA-256 を検証 |
| [`yo_to_rshogi_params`](yo_to_rshogi_params.md) | YaneuraOu の値を rshogi 形式の params に変換 |
| [`rshogi_to_yo_params`](rshogi_to_yo_params.md) | マッピングを使って rshogi の params を YaneuraOu 形式に変換 |
| [`build_param_mapping`](build_param_mapping.md) | YO と rshogi の params ペアからマッピング候補 TOML を生成 |
| [`check_param_mapping`](check_param_mapping.md) | マッピング表・params ペア・YO の USI option を照合 |
| [`spsa_stats_to_plot_csv`](spsa_stats_to_plot_csv.md) | 同期 / 非同期 SPSA の統計を描画用 CSV に変換 |
| [`spsa_test_engine`](../src/bin/spsa_test_engine.rs) | SPSA の検証用 USI エンジン（実験用エンジンではありません） |

## 形式・運用の共通資料

- [PSV 処理の選び方](pack_tools.md)、[教師データの追加パイプライン](training_data_pipeline.md)。
- [HCPE の形式](hcpe-format.md)、[LayerStacks routing・進行度係数](nnue-routing.md)。
- [SPSA の運用手順](spsa_runbook.md)、[コマンド例集](../examples/README.md)。
- CSA 接続は別 crate の [csa_client](../../rshogi-csa-client/examples/README.md) を使います。
- CSA の未決結果・SPRT 対象の制約は [analyze_selfplay](analyze_selfplay.md)、
  出力の保護・上書き・再開条件は各ツールの doc を参照してください。

### ラベル評価ツールの出力保護

`yardstick_label`、`label_bench_positions`、`label_bench_dl`、`yardstick_score` は、
出力と読み取り専用資源の同一実体への参照（alias）を拒否します。保護対象は各ツールの
「出力先の保護」に記載しています。出力パス自体が symlink の場合は参照先によらず拒否します。
hardlink と Windows の大小文字違いも検査します。

すべての出力を切り詰めずに開き、開いたファイルの同一性を確認してから既存出力を空にします。
複数出力の後続パスが資源や別出力と衝突する場合、先行する既存出力も保持します。
検査や open に失敗した際、新しく作られた空ファイルが残る場合があります。

この検査はファイルを排他ロックしません。実行中は別プロセスによる資源・出力の変更やパスの差し替えを
避けてください。衝突しない既存出力は上書きされ、切り詰め開始後の I/O エラーや処理中断では復元しません。
完了済みの結果を残すには、実行ごとに別の出力先を指定してください。
