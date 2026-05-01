# `tune/` ディレクトリ

YaneuraOu (YO) の SPSA チューニングと rshogi の SPSA を相互運用するための
**正本マッピング表** と、**ユーザがローカルに配置する YaneuraOu 由来の作業ファイル**
を置く場所。

## 含まれるもの

### 追跡対象 (リポジトリにコミット)

- **`yo_rshogi_mapping.toml`** — YaneuraOu と rshogi の SPSA パラメータ命名対応 102 エントリ
  の正本。`tune/suisho10.tune` のソース文脈と `crates/rshogi-core/src/search/tune_params.rs`
  の実装をクロスレビューして人手確定したもの。`yo_to_rshogi_params` /
  `rshogi_to_yo_params` / `check_param_mapping` / `spsa --engine-param-mapping` から参照される。
  詳細は `crates/tools/docs/spsa_runbook.md` の §10 を参照。

### 追跡対象外 (ユーザがローカルに配置、`.gitignore` 済み)

YaneuraOu の SPSA 用ツールチェインと、それに付随する `.tune` / `.params` ファイル群。
本リポジトリでは持たず、ユーザが必要に応じて取得・配置する:

| ファイル | 内容 | 入手元 |
|---|---|---|
| `tune.py` | YO ソースに `TUNE(...)` マクロを注入 / 値焼き戻しを行う Python スクリプト | [YaneuraOu-ScriptCollection/SPSA](https://github.com/yaneurao/YaneuraOu-ScriptCollection/tree/main/SPSA) |
| `ParamLib.py` | `tune.py` の補助モジュール | 同上 |
| `suisho10.tune` | suisho10 のチューニング対象を `@` マーカー付きで宣言した C++ 断片テンプレート | 同上 |
| `suisho10.params` | suisho10 の各チューニングパラメータの現在値・min/max/step | 同上 |

これらは YaneuraOu 上流側の資産であり、本リポジトリは利用するだけ。バージョンによって
内容が変わるので、ユーザが自身の YO ビルド構成に合わせた版を取得すること。

## 使い方

### YaneuraOu スクリプト一式の配置

YaneuraOu-ScriptCollection を clone or download し、`SPSA/` 直下の必要ファイルを
`tune/` にコピーする。例:

```bash
git clone https://github.com/yaneurao/YaneuraOu-ScriptCollection.git /tmp/yo-sc
cp /tmp/yo-sc/SPSA/{tune.py,ParamLib.py,suisho10.tune,suisho10.params} \
   /path/to/rshogi/tune/
```

ファイル一覧:

```text
tune/
├── README.md                  ← このファイル (追跡)
├── yo_rshogi_mapping.toml     ← 正本マッピング (追跡)
├── tune.py                    ← ユーザ配置 (追跡外)
├── ParamLib.py                ← ユーザ配置 (追跡外)
├── suisho10.tune              ← ユーザ配置 (追跡外)
└── suisho10.params            ← ユーザ配置 (追跡外)
```

### YaneuraOu バイナリへの SPSA パッチ適用 / 焼き戻し

`tune.py tune` で YO ソースに `TUNE(...)` マクロを注入し USI option として顕在化、
`tune.py apply` で SPSA 結果を実定数として焼き戻す。具体手順は
`crates/tools/docs/spsa_runbook.md` §10.6.0 〜 §10.6.3 参照。

### rshogi 形式 ⇔ YaneuraOu 形式の `.params` 変換

`yo_rshogi_mapping.toml` を介して双方向変換できる。詳細は同 runbook §10 参照。

```bash
# YO 形式 → rshogi 形式
cargo run --release -p tools --bin yo_to_rshogi_params -- \
  --yo-params tune/suisho10.params \
  --base spsa_params/<rshogi_base>.params \
  --mapping tune/yo_rshogi_mapping.toml \
  --output spsa_params/from_yo.params

# rshogi 形式 → YO 形式
cargo run --release -p tools --bin rshogi_to_yo_params -- \
  --rshogi-params spsa_params/<rshogi_tuned>.params \
  --base tune/suisho10.params \
  --mapping tune/yo_rshogi_mapping.toml \
  --output /tmp/tuned_yo.params
```

#### rshogi default 値の検知

入力 rshogi `.params` の値列が `SearchTuneParams::option_specs()` の default と
95% 以上一致した場合、`rshogi_to_yo_params` は警告を出す。これは
`generate_spsa_params` の出力 (= rshogi 内部 default 値) を canonical の代わりに
誤投入するのを防ぐためのチェック。

挙動とフラグ:

| 状況 | デフォルト | `--allow-rshogi-defaults` | `--strict-rshogi-defaults` |
|---|---|---|---|
| default と <95% 一致 | 通常変換 | 通常変換 | 通常変換 |
| default と ≥95% 一致 | warn 出力 + 続行 | 警告抑制して続行 | error で停止 |

意図的に default 値から始めたい場合は `--allow-rshogi-defaults` を、CI で混入を
完全に防ぎたい場合は `--strict-rshogi-defaults` を指定する。両者の同時指定は
意味が矛盾するため bail。

### マッピング表の整合性検証

```bash
cargo run --release -p tools --bin check_param_mapping -- \
  --mapping tune/yo_rshogi_mapping.toml \
  --yo-params tune/suisho10.params \
  --rshogi-params spsa_params/suisho10_converted.params \
  --yo-binary /path/to/YaneuraOu-tune-patched
```

## SPSA への投入時の注意

`yo_to_rshogi_params` / `rshogi_to_yo_params` の出力 (例: `from_rshogi.params`,
`from_yo.params`) は ラウンドトリップ確認や `tune.py apply` 前の焼き戻し用途で
あり、SPSA に直接の入力として使う際は **canonical (起点) として `--init-from`
に渡す** こと。SPSA の live 状態は `--run-dir <dir>` 配下の `state.params` に
書かれるため、canonical 自体が上書きされることはない。

```bash
# 正しい運用: canonical を --init-from に、SPSA の作業領域は --run-dir に分離
spsa --run-dir "runs/spsa/$(date -u +%Y%m%d_%H%M%S)" \
     --init-from tune/suisho10.params \
     --total-pairs 6400 --batch-pairs 32 --seed 1 \
     ...
```

v4 では `--total-pairs N` (SPSA 全体の game pair 数) と `--batch-pairs B`
(1 batch あたりの game pair 数、既定 8) が主役 CLI。multi-seed 機能
(`--seeds` / `--parallel-seeds`) は撤去された (詳細は CHANGELOG の v4 エントリ)。
詳細は `crates/tools/docs/spsa_runbook.md` §3 / §4.1 参照。

## 関連ドキュメント

- `crates/tools/docs/spsa_runbook.md` — SPSA 実行 runbook (本ディレクトリの全コマンド例 + トラブルシューティング)
- `spsa_params/` — rshogi 形式 `.params` の保管場所 (`.gitignore` 済み)
- `tune/yo_rshogi_mapping.toml` 冒頭コメント — マッピング表のフォーマット説明 (sign_flip, unmapped セクション等)
