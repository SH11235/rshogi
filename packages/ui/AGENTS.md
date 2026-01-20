# UI Components Guidelines

## CSS / スペーシング

### コンポーネントにマージンを持たせない（アンチパターン）

コンポーネント自身に `margin` (`m-*`, `mt-*`, `mb-*` 等) を持たせるのはアンチパターン。

**問題点:**
- 親レイアウトでの配置が予測困難になる
- 表示/非表示の切り替えでレイアウトシフトが発生する
- 別の場所での再利用時にマージンが邪魔になる

**NG例:**
```tsx
// コンポーネント内部にマージン
function MyComponent() {
  return <div className="mt-4 mb-2">...</div>;
}

// 親で使う時にマージンが固定されてしまう
<MyComponent />
```

**OK例:**
```tsx
// コンポーネントはマージンを持たない
function MyComponent({ className }: { className?: string }) {
  return <div className={cn("...", className)}>...</div>;
}

// 親がgapで制御
<div className="flex flex-col gap-4">
  <MyComponent />
  <AnotherComponent />
</div>

// または親からclassNameでスペーシングを渡す
<MyComponent className="mt-4" />
```

### レイアウトシフト対策

表示/非表示が切り替わる要素は、高さを常に確保する:

1. **プレースホルダー方式**: `visibility: hidden` で非表示にしつつスペースを確保
2. **固定高さ方式**: 親コンテナに `min-height` を設定
3. **常時表示方式**: 非アクティブ時はグレーアウト (`opacity-50` 等)

```tsx
// 高さ確保用のダミー要素
<div className="invisible" aria-hidden="true">
  <SameHeightElement />
</div>
```
