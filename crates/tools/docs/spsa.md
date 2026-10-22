# spsa

USIエンジンのパラメータを対局結果から調整する。起点を `--init-from`、作業領域を
`--run-dir` に分ける。ビルド、対局条件、再開、停止は [実行runbook](spsa_runbook.md) を参照。

## 入力形式

パラメータ行より前に、次の形式宣言を1回だけ置く。

```text
# spsa-format: c-end-r-end-v1
name,int,10,0,100,5,0.002
```

列は `name,type,value,min,max,c_end,r_end`。`c_end` は終端摂動幅、`r_end` は
終端学習率係数であり、旧形式の `step,delta` と同じ意味ではない。
空行、`#` コメント、行末の `//` コメント、`[[NOT USED]]` は利用できる。
生成される `state.params` と `final.params` にも形式宣言が付く。

宣言なし、未知の宣言、重複・行途中の宣言は既定で拒否する。列7が小さいことだけでは
現形式と判定できないため、数値による自動判別や自動補正はしない。

`--init-from` の全行とengine mappingは、`--force-init` による既存成果物の変更前に
検証する。非有限値、数値の構文・範囲不正、名前や翻訳先の重複、空の入力は拒否し、
既存のstate・meta・CSV・finalを保持する。`--meta-file` の指定先も同様に保持する。

## 既存ファイルの利用

列6/7が既に `c_end/r_end` と確認できた未宣言ファイルには、
`--assume-c-end-r-end` を明示できる。このフラグは `--init-from` と既存
`state.params` の未宣言入力だけに適用され、数値や元ファイルを変更しない。
未知・旧形式を宣言しているファイルは、このフラグを指定しても拒否する。

既存runを再開するときは、`state.params` と `meta.json` を手編集せず
`--resume --assume-c-end-r-end` を使う。通常のハッシュ・schedule検査は引き続き有効。
次のcheckpointから形式宣言が保存され、ハッシュも通常どおり更新される。

旧 `step,delta` ファイルはこのフラグで読み替えない。値列を新canonicalへ移し、
`c_end/r_end` を明示的に設計し直して新runを開始する。
[形式と移行の詳細](spsa_runbook.md#7-params-ファイルの形式と移行) を参照。
