# rshogi tools リファレンス

[クイックスタート](../README.md)から始め、用途に合うツールの詳細を開いてください。
一覧は `src/bin/` の全バイナリを含みます。個別 doc がない補助ツールはソースへリンクしています。
全オプションは `cargo run --release -p tools --bin <名前> -- --help` で確認できます。

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
| [`rescore_psv`](rescore_psv.md) | NNUE / 探索 / 外部 USI / ONNX で再評価し、再開・score sidecar に対応。探索はスレッド数によらず同じ出力 |
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
| [`migrate_psv_move16`](migrate_psv_move16.md) | 旧リポジトリ形式の move16 を YaneuraOu 形式に移行 |
| [`psv_dedup`](psv_dedup.md) | 64 bit hash 集合で重複除去。メモリは件数比例、hash 衝突の可能性あり |
| [`psv_dedup_bloom`](psv_dedup_bloom.md) | 固定サイズ Bloom filter による近似重複除去・差分抽出 |
| [`psv_dedup_partition`](psv_dedup_partition.md) | ディスク分割による exact 重複除去と任意の fused shuffle |
| [`psv_dedup_check`](psv_dedup_check.md) | 重複率を集計。hash 集合または固定サイズ近似テーブルを使用 |

## 対局・棋譜・外部連携

| ツール | 用途 |
|---|---|
| [`tournament`](tournament.md) | 複数エンジンの総当たり・先後交換対局、SPRT、動的制御、NPS 時計補正・Linux NUMA 固定 |
| [`analyze_selfplay`](analyze_selfplay.md) | 対局 JSONL の勝率・Elo・SPRT・開始局面別成績を集計 |
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
| [`mate_bench`](mate_bench.md) | df-pn 詰み探索の正解・探索量・時間を集計。最短手順 (詰将棋ルール / 全応手) と作意手数との照合 |
| [`compare_nodes`](compare_nodes.md) | 2 エンジンの深度別ノード数を比較し、通信終了をエラーとして検出 |
| [`compare_eval_nnue`](compare_eval_nnue.md) | PSV をサンプリングして NNUE の静的評価・探索スコアを比較 |
| [`eval_sfens`](eval_sfens.md) | SFEN 局面集を NNUE で静的評価 |
| [`nnue_saturation`](nnue_saturation.md) | LayerStacks の活性飽和率と bucket 分布を計測 |
| [`verify_nnue_accumulator`](verify_nnue_accumulator.md) | NNUE の差分更新と再計算の一致を検証 |
| [`dump_effect_bucket_golden`](dump_effect_bucket_golden.md) | effect bucket の active index を設定別に dump |
| [`prepack_nnue`](prepack_nnue.md) | LayerStacks FT を事前展開して共有読み込み用コンテナを生成 |
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
| [`book_from_csa`](book_from_csa.md) | CSA 棋譜から即指し・レート・勝敗で絞ったテキスト定跡を生成 |
| [`book_kachi_label`](book_kachi_label.md) | CSA の宣言勝ち決着率を定跡の候補手へ付与 |
| [`book_prepwalk`](book_prepwalk.md) | 相手の準備を自分の定跡で辿り、出口と準備の残り手数を集計 |
| [`book_rescore`](book_rescore.md) | 候補手を USI / ONNX で評価し、内容指紋で再開を検査 |
| [`book_extend`](book_extend.md) | USI bestmove を count=0 で候補集合へ追加 |
| [`book_backprop`](book_backprop.md) | 千日手・連続王手を考慮した後退解析で評価値を逆伝播 |
| [`book_mine`](book_mine.md) | 定跡の展開・借用 (借りる定跡は `.db` / `.ybb`。自分の読みで値を付けて `source=ext`・`lineage=book:<ファイル名>@<sha256 の先頭 8 文字>` で元データに登録)・元データからの書き出し (`master export`: DB・2 種のプリセット・注釈、source の語彙検査、読みの集計)・DB の差分 (`master diff`)・元データの全件検査 (`master validate`)・書き出した定跡とプリセットの検査 (`master check`)・監査 (main の点検 (元データに無い相手の手番の局面を補って伝播する `--fill-replies` つき)・反駁・再合流・循環を塞ぐ手。外す手の数が最小の組、外してよい手の制限つき)・読み始める局面と重みの辿り と、キュー・台帳を使う並列探索 |

## SPSA・パラメータ

| ツール | 用途 |
|---|---|
| [`spsa`](spsa.md) | 永続エンジンプールで同期 / 非同期 SPSA を実行し、途中再開 |
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
