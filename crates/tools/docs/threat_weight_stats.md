# threat_weight_stats

LayerStacks Threat モデルの量子化 FT 重みを読み、Threat のペアクラス別と駒 FT 全体の統計を TSV で標準出力に書きます。

モデルと同じ LayerStacks 寸法、FT、Threat 幅、除外 profile を選んでビルドします。1536×16×64、HalfKA_hm merged、Threat 幅 512 の例です。

```bash
cargo build --release -p tools --bin threat_weight_stats --no-default-features \
  --features layerstack-arch,layerstacks-1536x16x64,ft-halfka_hm_merged,nnue-threat,rshogi-core/edition-layerstacks-halfka_hm_merged-1536x16x64-threat512
target/release/threat_weight_stats "$SHOGI_DATA/nnue/model.bin"
```

全幅モデルには末尾の `-threat512` を `-threat` に変更します。1536×16×32 なら寸法 feature と preset の両方で `1536x16x64` を `1536x16x32` に置き換えます。除外 profile 付きのモデルには対応する `threat-profile-*` も指定します。

| 列 | 意味 |
|---|---|
| `pair_class` | 探索カウンタと同じ `攻撃駒番号_被攻撃駒番号_same` / `…_opp`。駒 FT 全体は `piece` |
| `rows` / `width` | 格納重みの行数 / 行あたりの要素数。Threat は実際に格納する幅、駒 FT は L1 |
| `mean_abs` / `rms` | 量子化重みの絶対値平均 / 二乗平均平方根 |
| `mean_row_l2` | 行ごとの L2 ノルムの平均 |

番号は 0=歩、1=香、2=桂、3=銀、4=金相当、5=角、6=飛、7=馬、8=竜です。同陣営は `same`、異陣営は `opp` です。除外されたクラスは `rows=0`、統計値は `NA` になります。集計対象はモデルに格納される全行です。
