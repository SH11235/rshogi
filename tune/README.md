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

### マッピング表の整合性検証

```bash
cargo run --release -p tools --bin check_param_mapping -- \
  --mapping tune/yo_rshogi_mapping.toml \
  --yo-params tune/suisho10.params \
  --rshogi-params spsa_params/suisho10_converted.params \
  --yo-binary /path/to/YaneuraOu-tune-patched
```

## 注意: 中間 `.params` ファイルを SPSA `--params` に直接渡さない

`yo_to_rshogi_params` / `rshogi_to_yo_params` の出力ファイル (例:
`from_rshogi.params`, `from_yo.params`) は **ラウンドトリップ確認や apply 前の
焼き戻し用** であり、SPSA の `--params` に直接渡すとそのファイルが反復ごとに
上書きされる。

過去 (2026-04) に、`rshogi_to_yo_params` の出力 (rshogi default 値が YO 名で
書かれたファイル) を SPSA に投入して 75,200 ゲーム規模のチューニングが台無し
になる事故が発生した。

**正しい運用**:
- 正本ファイル (`tune/suisho10.params` 等) は `--init-from` に指定する
- 反復用ファイルは `--params runs/spsa/<ts>/tuned.params` のように毎回
  timestamped dir に置く
- 起動時に出る `=== SPSA Startup Summary ===` で **init mode と上位 5 件の値**
  が想定通りかを目視確認する

詳細は `crates/tools/docs/spsa_runbook.md` §4.1 参照。

## 関連ドキュメント

- `crates/tools/docs/spsa_runbook.md` — SPSA 実行 runbook (本ディレクトリの全コマンド例 + トラブルシューティング)
- `spsa_params/` — rshogi 形式 `.params` の保管場所 (`.gitignore` 済み)
- `tune/yo_rshogi_mapping.toml` 冒頭コメント — マッピング表のフォーマット説明 (sign_flip, unmapped セクション等)
