# Threat テーブルサイズ一覧

L0 と Threat profile の組合せごとのテーブルサイズ。
L3 キャッシュとの関係を把握し、NPS 影響を見積もるための参考資料。

## L3 キャッシュサイズ (参考)

| アーキテクチャ | 代表 CPU | L3 (共有) |
|---|---|---|
| AMD Zen 3 | Ryzen 9 5950X (現環境) | 64 MB |
| AMD Zen 4/5 | Ryzen 9 7950X / 9950X | 64 MB |
| AMD 3D V-Cache | Ryzen 9 7950X3D / 9950X3D | 128 MB |
| Intel desktop | i9-13900K / 14900K | 36 MB |
| サーバー (EPYC/Xeon) | — | 128-384 MB |

## Threat テーブルサイズ (i8, L0 × dims bytes)

| profile | dims | L0=512 | L0=768 | L0=1024 | L0=1280 | L0=1536 |
|---------|-----:|-------:|-------:|--------:|--------:|--------:|
| Threat なし | 0 | 0 MB | 0 MB | 0 MB | 0 MB | 0 MB |
| enemy-only (未実装) | 48,160 | 24 MB | 35 MB | 47 MB | 59 MB | 71 MB |
| **cross-side** | **96,320** | **47 MB** | 71 MB | 94 MB | 118 MB | 141 MB |
| same-class (profile 1) | 192,640 | 94 MB | 141 MB | 188 MB | 235 MB | 282 MB |
| full (profile 0) | 216,720 | 106 MB | 159 MB | 212 MB | 265 MB | 317 MB |

## L3 キャッシュと cache pressure

### 探索中の主要メモリ消費（1ワーカーあたり）

Threat テーブルだけでなく、以下のデータ構造が L3 キャッシュを共有する。

| データ構造 | サイズ | アクセスパターン |
|---|---:|---|
| TT | 256〜1024 MB（設定値） | ランダム、1ノード1回 probe |
| ContinuationHistory | ~51 MB | move pair でインデックス、頻繁 |
| PawnHistory | ~41 MB | pawn hash + piece/sq、頻繁 |
| HalfKA_hm FT weights | 72〜215 MB（L0依存） | 差分更新で1手2-4行、局所的 |
| Threat テーブル | 0〜317 MB（profile/L0依存） | 20-40個の散在アクセス/局面 |
| その他 History | ~1 MB | 小さい |

History 群だけで ~92 MB あり、L3=64 MB を既に超えている。
したがって **「Threat テーブルが L3 に収まるか」だけでは判断できない**。

### なぜ Threat テーブルのサイズ削減が有効か

キャッシュは全か無ではなく、データが小さいほど cache pressure が減り、
他のデータ（History 群、FT weights）のキャッシュラインが追い出されにくくなる。

データ構造ごとのアクセス特性の違いが重要:

- **FT weights**: 差分更新で 1 手あたり 2-4 行のみアクセス。temporal locality が高く、
  テーブル全体が L3 に載る必要はない
- **History 群**: move に依存するためある程度の局所性がある
- **Threat テーブル**: active feature が局面あたり 20-40 個で散在し、局所性が低い。
  テーブルサイズがそのまま cache pollution の大きさに直結する

Threat テーブルは局所性が最も低いため、サイズ削減の NPS 改善効果が最も大きい。
L3 サイズは cache pressure の相対的な目安として参照する。

### Threat テーブルの L3 サイズ対比

- L3=64 MB: enemy-only × L0=1280 以下、cross-side × L0=512 (47 MB)
- L3=128 MB: enemy-only × 全サイズ、cross-side × L0=1024 以下、same-class × L0=512

## HalfKA_hm FT weights (i16, L0 × dims × 2 bytes)

HalfKA_hm dims = 73,305

| L0 | FT weights |
|---:|----------:|
| 512 | 72 MB |
| 768 | 107 MB |
| 1024 | 143 MB |
| 1280 | 179 MB |
| 1536 | 215 MB |

## FT 合計サイズ (HalfKA_hm + Threat)

| profile | L0=512 | L0=768 | L0=1024 | L0=1280 | L0=1536 |
|---------|-------:|-------:|--------:|--------:|--------:|
| Threat なし | 72 MB | 107 MB | 143 MB | 179 MB | 215 MB |
| enemy-only (未実装) | 96 MB | 142 MB | 190 MB | 238 MB | 286 MB |
| cross-side | 119 MB | 178 MB | 237 MB | 297 MB | 356 MB |
| same-class (profile 1) | 166 MB | 248 MB | 331 MB | 414 MB | 497 MB |
| full (profile 0) | 177 MB | 266 MB | 355 MB | 444 MB | 532 MB |

## Stockfish (参考)

| 項目 | 値 |
|------|-----|
| HalfKAv2_hm dims | 22,528 |
| Threat dims | 60,720 |
| L0 | 1,024 |
| HalfKAv2_hm FT weights (i16) | 44 MB |
| Threat テーブル (i8) | 59 MB |
| FT 合計 | 103 MB |

Stockfish の Threat テーブル (59 MB) は L3=64 MB 以下。
rshogi の full profile (L0=768) の Threat テーブルは Stockfish の 2.7 倍。

## NPS 実測データ

| 構成 | NPS | v87 比 | Threat テーブル |
|------|-----|--------|---------------|
| v87 (L0=1536, Threat なし) | 254K | 100% | 0 MB |
| v93 (L0=768, profile 1) | ~193K | ~76% | 141 MB |
| v91 (L0=768, profile 0) | 189K | 74.5% | 159 MB |
| v92 (L0=512, profile 0) | 227K | —* | 106 MB |
| v94 (L0=512, cross-side) | 254K | —* | 47 MB |
| v89 (L0=1536, profile 0) | 306K** | 54%** | 317 MB |

*v87 比は L0=1536 基準のため、L0 差による FT 計算コスト削減分が混入する。
**v89 は最適化前の値

### 同一 L0=512 での profile 比較 (v92 vs v94, selfplay 暫定値)

production build + byoyomi 1000ms selfplay (40局時点) から:

| 構成 | Threat テーブル | NPS | avg_depth |
|------|---------------:|----:|----------:|
| v92 (profile 0, full) | 106 MB | 226,540 | 16.70 |
| v94 (cross-side) | 47 MB | 253,904 | 17.07 |

Threat テーブル 106 MB → 47 MB の削減で NPS 約 **+12%**、depth も +0.37 向上。

ただし棋力は 225 局時点で Elo +14 ±45 (v94 負け越し方向)。cross-side は
情報量を半減しているため eval 品質低下が NPS 改善を上回っている。

## perf 計測による寄与分解 (2026-04-12)

selfplay ベースの NPS 差だけでは「計算量削減」と「cache pressure 削減」を
切り分けられない。`search_only_ab` で cycles/node・instructions/node・CPI を
計測し、両者の寄与を分解した。

**計測ツール**: `crates/tools/src/bin/search_only_ab.rs` (perf stat --control 方式、
初期化コストを完全排除、`abba` 順序で順序バイアス補正)

詳細と再現コマンドは [threat_table_cpi_measurement_20260412.md](./threat_table_cpi_measurement_20260412.md) を参照。

### CPI (cycles/node ÷ instructions/node) 一覧

CPI は命令数の違いを除いた **cache pressure の純粋な指標**。stall が多いほど増加する。

| 構成 | Threat テーブル | cycles/node | instructions/node | **CPI** | vs v87 CPI |
|---|---:|---:|---:|---:|---:|
| v87 (L0=1536, Threat なし) | 0 MB | 7,424 | 15,860 | **0.468** | — |
| **v94 (L0=512, cross-side)** | **47 MB (L3 以下)** | 8,741 | 18,493 | **0.473** | **+1.1%** |
| v92 (L0=512, profile 0) | 106 MB | 9,555 | 19,651 | 0.486 | +3.8% |
| v93 (L0=768, profile 1) | 141 MB | 10,569 | 21,933 | 0.482 | +2.9% |
| v91 (L0=768, profile 0) | 159 MB | 10,804 | 21,935 | 0.493 | +5.3% |

### v92 → v94 の NPS +9.1% の内訳

`search_only_ab` 計測 (movetime 10s, rounds 2, 4局面) から:

- **instructions/node -5.88%**: dims 半減 (216,720 → 96,320) → 計算量削減
- **CPI -2.7%** (0.486 → 0.473): cache pressure 削減
- **寄与比: 計算量 : cache ≈ 2 : 1**

**selfplay ベースでの「NPS +12% は cache 削減効果を切り分けたデータ」という
当初の解釈は誤りだった**。実際は 2/3 が計算量削減、1/3 が cache pressure 削減。

### テーブルサイズと CPI の関係

v87 (Threat なし) との CPI 差で cache pressure 追加分を見ると:

- L3 以下 (v94, 47 MB): CPI +1.1%
- L3 超過 (v92, 106 MB): CPI +3.8%
- L3 超過 (v93, 141 MB): CPI +2.9%
- L3 超過 (v91, 159 MB): CPI +5.3%

テーブルサイズが小さいほど CPI 増加が小さい傾向はあるが、**単調ではない**。
v92 (106 MB) と v93 (141 MB) の逆転も観測された。profile 内容や L0 との
相互作用が影響している。

v92 → v94 の CPI -2.8% (NPS 換算 +2.8% 相当) は、「テーブル 106 MB → 47 MB」と
「profile 0 → cross-side」の**両方の効果を含む**。**「L3 以下にする単独効果」を
厳密に切り出すには同一 profile での L3 境界跨ぎ比較が必要で、今回のデータからは
分離できない**。

ペアの選び方で CPI 改善幅は変動する:

- v91 (159 MB) → v94 (47 MB): CPI -4.2% (NPS +4.2% 相当)
- v92 (106 MB) → v94 (47 MB): CPI -2.8% (NPS +2.8% 相当)
- v93 (141 MB) → v94 (47 MB): CPI -1.9% (NPS +1.9% 相当)

現データから言えるのは「**100 MB 台 → 50 MB 未満の範囲のテーブルサイズ削減で、
cache 関連の寄与は NPS +2〜4% 程度のオーダー**」という目安のみ。

### 結論: 実験方針への示唆

1. **テーブルサイズ削減の cache 関連寄与は NPS +2〜4% オーダー**
   (100 MB 台 → 50 MB 未満の範囲で観測。同一 profile での L3 境界跨ぎ比較がないため、
   「L3 以下にする単独効果」は本計測からは厳密には分離できない)
2. **instructions/node 削減のほうが支配的** — Threat 導入の NPS コスト (v87 比
   -15〜-32%) のうち、**計算量増 (+17〜+38%) が主因**で、cache pressure (+1〜+5%) は従
3. **cross-side のような極端な dims 削減は避けるべき** — eval 品質低下のペナルティが
   cache 削減効果 (+1.1% CPI) を遥かに上回る
4. **次に試すべき方向**:
   - **A. 中間 dims の設計**: full (216,720) と cross-side (96,320) の間で、
     重要な pair だけ残すサブセット (例: 130k〜160k dims)
   - **B. Accumulate 処理の SIMD/vectorize 最適化**: dims と eval 品質を保ちつつ
     cycles/node を削減
   - **C. Cache-friendly レイアウト**: feature index 並び替え、cache line align、
     prefetch で CPI を単独で改善

---
作成日: 2026-04-12
