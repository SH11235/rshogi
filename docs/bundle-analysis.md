# バンドルサイズ分析記録

このドキュメントはWebアプリのバンドルサイズ分析結果と、削減施策の判断記録です。

## 現在のバンドルサイズ (2025-12-26)

| ファイル | 生サイズ | gzip後 |
|----------|----------|--------|
| index.js | 437 KB | 139 KB |
| engine_wasm_bg.wasm | 427 KB | 198 KB |
| index.css | 47 KB | 9 KB |
| **合計** | **911 KB** | **346 KB** |

## 適用済みの最適化

### wasm-opt 適用

- **実施日**: 2025-12-26
- **効果**: WASM 635KB → 427KB (35%削減)
- **gzip後**: 201KB → 198KB (わずかな改善)
- **実装**: `packages/engine-wasm/scripts/build-wasm.mjs` に組み込み

## 検討して不採用とした施策

### tailwind-merge → clsx 置換

- **検討日**: 2025-12-26
- **期待効果**: 約90KB削減 (gzip後 15.5KB)
- **判断**: 不採用

**理由**:
1. `cn()` 関数はshadcn/uiパターンで、外部classNameとのマージに使用
2. tailwind-mergeはTailwindクラスのConflict解決を行う（例: `cn("p-4", "p-8")` → `"p-8"`）
3. clsxのみだと両方のクラスが残り、CSSの適用順序に依存した不安定な挙動になる
4. gzip後15.5KBの削減のためにスタイルの信頼性を犠牲にするのは割に合わない

**使用箇所の例**:
```tsx
// packages/ui/src/components/button.tsx
className={cn(buttonVariants({ variant, size }), className)}

// packages/ui/src/components/dialog.tsx
className={cn("flex flex-col space-y-1.5", className)}
```

**テストで確認済みのConflict解決**:
```ts
// packages/design-system/src/lib/cn.test.ts
expect(cn("p-4", "p-8")).toBe("p-8");
expect(cn("text-red-500", "text-blue-500")).toBe("text-blue-500");
```

### @radix-ui 未使用コンポーネント削除

- **検討日**: 2025-12-26
- **期待効果**: コンポーネントあたり 5-20KB削減
- **判断**: 対象なし

**理由**:
- knipで未使用パッケージとして検出されていない
- 現在使用中のRadixコンポーネント: Tooltip, Dialog, Collapsible, RadioGroup等
- すべて実際にUIで使用されている

## 今後検討可能な施策

### コード分割 (lazy loading)

- **期待効果**: 初期ロード 30-50KB削減
- **対象候補**:
  - EvalGraphModal (11.6KB)
  - MatchSettingsPanel (15.8KB)
  - その他モーダル系コンポーネント
- **難易度**: 中
- **状態**: 未着手

### バンドル内訳 (参考)

node_modules上位:
| パッケージ | サイズ | 割合 |
|------------|--------|------|
| react-dom | 548 KB | 45.8% |
| tailwind-merge | 92 KB | 7.7% |
| @floating-ui | 61 KB | 5.1% |
| @radix-ui (合計) | ~100 KB | 8% |

packages/*:
| パッケージ | サイズ |
|------------|--------|
| packages/ui | 255 KB |
| packages/engine-wasm | 31 KB |
| packages/app-core | 20 KB |

## 更新履歴

| 日付 | 内容 | gzip後合計 | 目標達成 |
|------|------|------------|----------|
| 2025-12-26 | wasm-opt適用、初回記録 | 346 KB | ✓ (< 360KB) |
