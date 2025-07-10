# 定跡データディレクトリ

[user_book1.db](https://github.com/yaneurao/YaneuraOu/releases/tag/new_petabook233)から変換された定跡データ（.binzファイル）を配置します。

## 必要なファイル

以下のファイルが必要です：
- `opening_book_early.binz` - 序盤用の軽量データ
- `opening_book_web.binz` - Web用の標準データ
- `opening_book_standard.binz` - 標準データ（webのコピー）

## 生成方法

```bash
# packages/rust-core ディレクトリで実行
make convert-opening-books-parallel
```

## CD環境での動作

- このディレクトリにファイルがある場合：自動的にWebパッケージにコピー
- ファイルがない場合：警告を出すがビルドは継続

## 注意事項

- .binzファイルはGit管理外です（.gitignoreで除外）
- 開発者は各自でデータを生成またはダウンロードする必要があります
- CD環境では別途データを提供する必要があります
