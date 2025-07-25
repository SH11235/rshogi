commit b4f91fe110137c1bae9977c15a24d6f28a1bc22e
$ cargo bench --bench buffering_benchmark --features buffered-io -- --sample-size 10 --warm-up-time 1 --measurement-time 3

Finished `bench` profile [optimized] target(s) in 0.07s
     Running benches/buffering_benchmark.rs (target/release/deps/buffering_benchmark-154921e53f345ae2)
Gnuplot not found, using plotters backend
buffered_io/immediate/3 time:   [103.95 ms 104.02 ms 104.08 ms]
                        thrpt:  [38.433  elem/s 38.454  elem/s 38.479  elem/s]
                 change:
                        time:   [-0.2164% -0.1306% -0.0539%] (p = 0.00 < 0.05)
                        thrpt:  [+0.0539% +0.1307% +0.2168%]
                        Change within noise threshold.
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) low mild
Benchmarking buffered_io/buffered_100ms/3: Collecting 20 samples in estimated 43.767 s (420                
buffered_io/buffered_100ms/3
                        time:   [103.86 ms 103.90 ms 103.95 ms]
                        thrpt:  [38.480  elem/s 38.498  elem/s 38.514  elem/s]
                 change:
                        time:   [-0.3254% -0.2164% -0.1219%] (p = 0.00 < 0.05)
                        thrpt:  [+0.1220% +0.2169% +0.3265%]
                        Change within noise threshold.
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) low mild
buffered_io/immediate/4 time:   [106.94 ms 107.00 ms 107.07 ms]
                        thrpt:  [46.699  elem/s 46.727  elem/s 46.755  elem/s]
                 change:
                        time:   [+0.3627% +0.5021% +0.6290%] (p = 0.00 < 0.05)
                        thrpt:  [-0.6251% -0.4996% -0.3614%]
                        Change within noise threshold.
Found 3 outliers among 20 measurements (15.00%)
  1 (5.00%) low mild
  2 (10.00%) high mild
buffered_io/buffered_100ms/4
                        time:   [106.96 ms 107.04 ms 107.10 ms]
                        thrpt:  [46.686  elem/s 46.713  elem/s 46.746  elem/s]
                 change:
                        time:   [+0.5026% +0.6290% +0.7530%] (p = 0.00 < 0.05)
                        thrpt:  [-0.7474% -0.6251% -0.5001%]
                        Change within noise threshold.
Found 2 outliers among 20 measurements (10.00%)
  1 (5.00%) low mild
  1 (5.00%) high mild
buffered_io/immediate/5 time:   [136.43 ms 136.69 ms 136.90 ms]
                        thrpt:  [43.828  elem/s 43.896  elem/s 43.979  elem/s]
                 change:
                        time:   [-0.1899% +0.0531% +0.2825%] (p = 0.64 > 0.05)
                        thrpt:  [-0.2817% -0.0530% +0.1902%]
                        No change in performance detected.
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high mild
buffered_io/buffered_100ms/5
                        time:   [137.00 ms 137.19 ms 137.32 ms]
                        thrpt:  [43.693  elem/s 43.735  elem/s 43.796  elem/s]
                 change:
                        time:   [-0.1349% +0.1642% +0.4506%] (p = 0.25 > 0.05)
                        thrpt:  [-0.4486% -0.1639% +0.1351%]
                        No change in performance detected.

time_based_search/immediate/100
                        time:   [202.53 ms 202.65 ms 202.80 ms]
time_based_search/buffered_100ms/100
                        time:   [202.46 ms 202.66 ms 202.79 ms]
time_based_search/immediate/200
                        time:   [293.09 ms 293.76 ms 294.99 ms]
Found 1 outliers among 10 measurements (10.00%)
  1 (10.00%) high mild
time_based_search/buffered_100ms/200
                        time:   [293.67 ms 295.08 ms 295.88 ms]
time_based_search/immediate/500
                        time:   [293.75 ms 295.12 ms 296.39 ms]
time_based_search/buffered_100ms/500
                        time:   [296.09 ms 297.59 ms 299.27 ms]


● ベンチマーク結果の分析

  🔍 主要な発見事項

  1. バッファリングによる性能差がほとんど見られない

  - depth 3: immediate (104.02ms) vs buffered (103.90ms) - 差: 0.12ms (0.1%)
  - depth 4: immediate (107.00ms) vs buffered (107.04ms) - 差: -0.04ms (-0.04%)
  - depth 5: immediate (136.69ms) vs buffered (137.19ms) - 差: -0.50ms (-0.4%)

  2. 時間ベース探索でも同様の傾向

  - 100ms: immediate (202.65ms) vs buffered (202.66ms) - 差: -0.01ms
  - 200ms: immediate (293.76ms) vs buffered (295.08ms) - 差: -1.32ms (-0.4%)
  - 500ms: immediate (295.12ms) vs buffered (297.59ms) - 差: -2.47ms (-0.8%)


📊 分析結果

バッファリングの効果が見られない理由：

1. I/O時間が総実行時間に占める割合が小さい
  - 探索処理が支配的で、I/O処理の改善効果が見えない
  - depth 3で4要素、depth 5で6要素のinfo出力では差が出にくい
2. OSレベルのバッファリング
  - OSがすでに効率的なバッファリングを行っている可能性
  - 小規模なwrite syscallでは顕著な差が出ない
3. 測定環境の影響
  - ローカル環境での実行（ネットワーク遅延なし）
  - SSDなど高速ストレージによる影響

💡 改善提案

1. より多くのinfo出力を生成する条件でテスト
// より深い探索、または時間を長くして info 出力を増やす
depth 10-15, または movetime 5000-10000
2. システムコール数の直接測定
  - 実行時間ではなく、実際のwrite syscall数を測定
  - straceやperfを使った測定結果をベンチマークに含める
3. ストレス条件での測定
// 同時に複数のエンジンを実行
// I/O負荷が高い状況をシミュレート
4. バッファサイズの調整
  - 現在の100msは長すぎる可能性
  - 10ms, 50ms, 100msなど複数の設定で比較

🎯 結論

現在のベンチマーク結果では、バッファリングによる顕著な性能改善は見られません。これは：

1. 測定条件が軽すぎる（info出力が少ない）
2. I/O時間が全体に占める割合が小さい
3. OSレベルの最適化が効いている

より現実的な効果測定のためには、高頻度のinfo出力やシステムコール数の直接測定が必要です。

📋 今後の改善提案（新規タスク候補）

ベンチマーク結果の分析から、以下の追加タスクが考えられます：

1. より現実的な測定条件の実装
- 深い探索（depth 10-15）でのベンチマーク追加
- 長時間探索（movetime 5000-10000ms）のシナリオ
2. システムコール数の直接測定
- Linux環境でstrace/perf統合によるsyscall数測定
- ベンチマーク結果にsyscall削減率を含める
3. 複数バッファサイズの比較
- 10ms, 50ms, 100ms, 200msなど複数の設定での測定
- 最適なバッファサイズの特定
4. CI統合（Phase 3の残り）
- GitHub Actionsでのベンチマーク自動実行
- パフォーマンス回帰検出の実装
