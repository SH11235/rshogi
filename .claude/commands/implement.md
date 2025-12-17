## Implement (TypeScript/汎用)

設計ドキュメントに基づいて実装を開始するコマンドです。
TypeScriptプロジェクト（ui, app-core, apps/*）向けです。

```
/implement <設計ドキュメントパス1> [設計ドキュメントパス2] ...
```

**Rust将棋エンジンの実装は `/implement-rust` を使用してください。**

### 使用例

```
/implement docs/ui-component-design.md
/implement docs/design1.md docs/design2.md
```

### 実装手順

1. まず各設計ドキュメントを読み込み、内容を理解してください
2. 実装に必要な調査や確認事項があれば、それらを先に解決してください
3. 必要に応じて設計ドキュメントを更新してください
4. タスクを適切なステップに分割し、TodoWriteツールで管理してください
5. 各ステップごとに:
   - 実装を行う
   - テストを作成・実行する
   - 完了したらユーザーに報告（ユーザーがgit commitを実行）
6. すべてのステップが完了するまで続けてください

### 検証コマンド

```bash
# lint & typecheck
pnpm lint
pnpm typecheck

# テスト
pnpm test

# 特定パッケージのみ
pnpm --filter @shogi/ui lint
pnpm --filter @shogi/ui typecheck
```

### 注意事項

- TDDの原則に従ってください（テストファースト）
- 型安全性を確保してください（anyタイプは使用禁止）
- 実装後は必ずlintとtypecheckを実行してください
- git操作はユーザーが行うので、コミットポイントで明確に報告してください
