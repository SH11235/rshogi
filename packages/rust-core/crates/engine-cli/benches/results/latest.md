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
