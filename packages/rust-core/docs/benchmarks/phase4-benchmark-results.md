# Phase 4 Benchmark Results - Four-Way Comparison

## Test Configuration
- **Date**: 2025-08
- **Benchmark**: `tt_prefetch_bench_v4`
- **Modes**: NoTT vs TT(no CAS) vs TTOnly vs TT+Prefetch
- **Purpose**: Isolate CAS overhead and prefetch impact

## Raw Benchmark Data

### DEPTH 4 RESULTS

#### Initial position (Depth 4)
```
Mode              Nodes        NPS Time(ms)   vs NoTT |  TT Hit% | Hashfull | Prefetch
----------------------------------------------------------------------------------------------------
NoTT             455823    3427240      133     +0.0%
TT (no CAS)      455823    3376466      135     -1.5% | TT: 0.0% | HF: 0.0%
TTOnly           286630    3372117       85     -1.6% | TT: 37.4% | HF: 0.1%
TT+Pref          286630    3372117       85     -1.6% | TT: 37.4% | HF: 0.1% | PF: 14.7% d=1

Analysis:
  Node reduction: 37.1%
  CAS overhead: -37.0%
  Prefetch benefit: 0.0%
  Prefetch hit rate: 14.7%
  Prefetch distance: 1
```

#### Standard opening (Depth 4)
```
Mode              Nodes        NPS Time(ms)   vs NoTT |  TT Hit% | Hashfull | Prefetch
----------------------------------------------------------------------------------------------------
NoTT              57685    3204722       18     +0.0%
TT (no CAS)       57685    3036052       19     -5.3% | TT: 0.0% | HF: 0.0%
TTOnly            43884    3134571       14     -2.2% | TT: 30.8% | HF: 0.0%
TT+Pref           43884    3134571       14     -2.2% | TT: 30.8% | HF: 0.0% | PF: 32.2% d=1

Analysis:
  Node reduction: 23.9%
  CAS overhead: -26.3%
  Prefetch benefit: 0.0%
  Prefetch hit rate: 32.2%
  Prefetch distance: 1
```

#### Middle game (Depth 4)
```
Mode              Nodes        NPS Time(ms)   vs NoTT |  TT Hit% | Hashfull | Prefetch
----------------------------------------------------------------------------------------------------
NoTT             172606    3452120       50     +0.0%
TT (no CAS)      172606    3384431       51     -2.0% | TT: 0.0% | HF: 0.0%
TTOnly           122805    3411250       36     -1.2% | TT: 31.3% | HF: 0.0%
TT+Pref          122805    3411250       36     -1.2% | TT: 31.3% | HF: 0.0% | PF: 42.7% d=1

Analysis:
  Node reduction: 28.9%
  CAS overhead: -29.4%
  Prefetch benefit: 0.0%
  Prefetch hit rate: 42.7%
  Prefetch distance: 1
```

#### Complex endgame (Depth 4)
```
Mode              Nodes        NPS Time(ms)   vs NoTT |  TT Hit% | Hashfull | Prefetch
----------------------------------------------------------------------------------------------------
NoTT           15344924    4438797     3457     +0.0%
TT (no CAS)    15344924    4383011     3501     -1.3% | TT: 0.0% | HF: 0.0%
TTOnly          8884498    4255027     2088     -4.1% | TT: 24.1% | HF: 1.9%
TT+Pref         8884498    4250955     2090     -4.2% | TT: 24.1% | HF: 1.9% | PF: 37.2% d=1

Analysis:
  Node reduction: 42.1%
  CAS overhead: -40.4%
  Prefetch benefit: -0.1%
  Prefetch hit rate: 37.2%
  Prefetch distance: 1
```

### DEPTH 5 RESULTS

#### Initial position (Depth 5)
```
Mode              Nodes        NPS Time(ms)   vs NoTT |  TT Hit% | Hashfull | Prefetch
----------------------------------------------------------------------------------------------------
NoTT            6165615    3425341     1800     +0.0%
TT (no CAS)     6165615    3347239     1842     -2.3% | TT: 0.0% | HF: 0.0%
TTOnly          2311518    3340343      692     -2.5% | TT: 40.9% | HF: 1.3%
TT+Pref         2311518    3321146      696     -3.0% | TT: 40.9% | HF: 1.3% | PF: 30.9% d=1

Analysis:
  Node reduction: 62.5%
  CAS overhead: -62.4%
  Prefetch benefit: -0.6%
  Prefetch hit rate: 30.9%
  Prefetch distance: 1
```

#### Standard opening (Depth 5)
```
Mode              Nodes        NPS Time(ms)   vs NoTT |  TT Hit% | Hashfull | Prefetch
----------------------------------------------------------------------------------------------------
NoTT             925940    3306928      280     +0.0%
TT (no CAS)      925940    3215069      288     -2.8% | TT: 0.0% | HF: 0.0%
TTOnly           551594    3225695      171     -2.5% | TT: 28.0% | HF: 0.4%
TT+Pref          551594    3225695      171     -2.5% | TT: 28.0% | HF: 0.4% | PF: 30.3% d=1

Analysis:
  Node reduction: 40.4%
  CAS overhead: -40.6%
  Prefetch benefit: 0.0%
  Prefetch hit rate: 30.3%
  Prefetch distance: 1
```

#### Middle game (Depth 5)
```
Mode              Nodes        NPS Time(ms)   vs NoTT |  TT Hit% | Hashfull | Prefetch
----------------------------------------------------------------------------------------------------
NoTT            3017828    3394632      889     +0.0%
TT (no CAS)     3017828    3294572      916     -2.9% | TT: 0.0% | HF: 0.0%
TTOnly          1883094    3292122      572     -3.0% | TT: 30.9% | HF: 1.1%
TT+Pref         1883094    3286376      573     -3.2% | TT: 30.9% | HF: 1.1% | PF: 41.5% d=1

Analysis:
  Node reduction: 37.6%
  CAS overhead: -37.6%
  Prefetch benefit: -0.2%
  Prefetch hit rate: 41.5%
  Prefetch distance: 1
```

#### Complex endgame (Depth 5)
```
Mode              Nodes        NPS Time(ms)   vs NoTT |  TT Hit% | Hashfull | Prefetch
----------------------------------------------------------------------------------------------------
NoTT          240297564    4199758    57217     +0.0%
TT (no CAS)   240297564    3949209    60847     -6.0% | TT: 0.0% | HF: 0.0%
TTOnly         74574075    3696360    20175    -12.0% | TT: 15.3% | HF: 79.8%
TT+Pref        74574075    3686675    20228    -12.2% | TT: 15.3% | HF: 79.8% | PF: 93.0% d=4

Analysis:
  Node reduction: 69.0%
  CAS overhead: -66.8%
  Prefetch benefit: -0.3%
  Prefetch hit rate: 93.0%
  Prefetch distance: 4
```

### DEPTH 6 RESULTS

#### Initial position (Depth 6)
```
Mode              Nodes        NPS Time(ms)   vs NoTT |  TT Hit% | Hashfull | Prefetch
----------------------------------------------------------------------------------------------------
NoTT          214417968    3433324    62452     +0.0%
TT (no CAS)   214417968    3372836    63572     -1.8% | TT: 0.0% | HF: 0.0%
TTOnly         37909361    3382952    11206     -1.5% | TT: 52.0% | HF: 21.8%
TT+Pref        37909361    3348883    11320     -2.5% | TT: 52.0% | HF: 21.8% | PF: 26.6% d=3

Analysis:
  Node reduction: 82.3%
  CAS overhead: -82.4%
  Prefetch benefit: -1.0%
  Prefetch hit rate: 26.6%
  Prefetch distance: 3
```

#### Standard opening (Depth 6)
```
Mode              Nodes        NPS Time(ms)   vs NoTT |  TT Hit% | Hashfull | Prefetch
----------------------------------------------------------------------------------------------------
NoTT           24253480    3375101     7186     +0.0%
TT (no CAS)    24253480    3292178     7367     -2.5% | TT: 0.0% | HF: 0.0%
TTOnly          8164053    3273477     2494     -3.0% | TT: 39.8% | HF: 6.4%
TT+Pref         8164053    3263010     2502     -3.3% | TT: 39.8% | HF: 6.4% | PF: 51.9% d=1

Analysis:
  Node reduction: 66.3%
  CAS overhead: -66.1%
  Prefetch benefit: -0.3%
  Prefetch hit rate: 51.9%
  Prefetch distance: 1
```

#### Middle game (Depth 6)
```
Mode              Nodes        NPS Time(ms)   vs NoTT |  TT Hit% | Hashfull | Prefetch
----------------------------------------------------------------------------------------------------
NoTT           21202579    3394041     6247     +0.0%
TT (no CAS)    21202579    3314974     6396     -2.3% | TT: 0.0% | HF: 0.0%
TTOnly          8909124    3288713     2709     -3.1% | TT: 36.4% | HF: 5.9%
TT+Pref         8909124    3279029     2717     -3.4% | TT: 36.4% | HF: 5.9% | PF: 27.3% d=1

Analysis:
  Node reduction: 58.0%
  CAS overhead: -57.6%
  Prefetch benefit: -0.3%
  Prefetch hit rate: 27.3%
  Prefetch distance: 1
```

## Key Findings Summary

### CAS Overhead Impact
| Depth | Min CAS Overhead | Max CAS Overhead | Average |
|-------|------------------|------------------|---------|
| 4 | -26.3% | -40.4% | -33.2% |
| 5 | -37.6% | -66.8% | -51.9% |
| 6 | -57.6% | -82.4% | -68.7% |

### Prefetch Effectiveness
| Depth | Best Case | Worst Case | Average |
|-------|-----------|------------|---------|
| 4 | 0.0% | -0.1% | -0.025% |
| 5 | 0.0% | -0.6% | -0.28% |
| 6 | -0.3% | -1.0% | -0.53% |

### Prefetch Hit Rates
- Range: 14.7% to 93.0%
- Higher hit rates do NOT correlate with better performance
- Even 93% hit rate resulted in -0.3% performance

## Conclusions

1. **CAS operations are the dominant performance factor**, not cache misses
2. **Prefetch provides no benefit** in current implementation
3. **Node reduction from TT is real** but completely offset by CAS overhead
4. **Single-threaded performance** is severely impacted by thread-safety mechanisms

---
*Raw data preserved for future analysis and comparison*
