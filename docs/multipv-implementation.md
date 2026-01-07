# MultiPV（複数読み筋）対応 実装ガイド

## 概要

検討モードでエンジン解析時に複数の候補手（読み筋）を表示できるようにする。
現在は最善手（MultiPV=1）のみだが、第2候補、第3候補なども表示・比較できるようにする。

## 現状調査結果

### エンジン側（対応済み）

エンジン・バックエンド層は既にMultiPV対応済み：

- `packages/rust-core/crates/engine-core/src/search/limits.rs`: `multi_pv: usize`
- `apps/desktop/src-tauri/src/lib.rs`: `MultiPV`オプション処理
- `packages/engine-client/src/index.ts`: `EngineInfoEvent.multipv?: number`
- エンジン設定UIにも`MultiPV`オプション定義あり

### 未対応箇所

UI/データ層でMultiPV対応が不足：

```
packages/
├── app-core/src/game/kifu-tree.ts
│   ├── KifuEval        # 単一PVのみ保持
│   └── KifuNode.eval   # 単一評価値のみ
├── ui/src/components/shogi-match/
│   ├── types.ts
│   │   └── AnalysisSettings  # multiPv設定なし
│   ├── hooks/
│   │   ├── useKifuNavigation.ts
│   │   │   └── recordEvalByPly  # multipv考慮なし
│   │   └── useEngineManager.ts
│   │       └── analyzePosition  # MultiPV設定なし
│   └── components/
│       └── KifuPanel.tsx
│           └── ExpandedMoveDetails  # 単一PV表示
```

## データフロー

```
エンジン (Rust)
    ↓ info multipv 1 score cp 100 pv 7g7f ...
    ↓ info multipv 2 score cp 80 pv 2g2f ...
Tauri IPC / WASM Worker
    ↓ EngineInfoEvent { multipv: 1, scoreCp: 100, pv: [...] }
    ↓ EngineInfoEvent { multipv: 2, scoreCp: 80, pv: [...] }
useEngineManager.ts
    ↓ onEvalUpdate(ply, event)  ← 現在はmultipv無視
shogi-match.tsx
    ↓ recordEvalByPly(ply, event)  ← 現在は最新で上書き
useKifuNavigation.ts
    ↓ setNodeEval(tree, nodeId, evalData)
kifu-tree.ts
    → KifuNode.eval に保存  ← 現在は単一のみ
```

## 実装方針

### Phase 1: データ構造の拡張

#### 1.1 KifuEval/KifuNodeの拡張

```typescript
// packages/app-core/src/game/kifu-tree.ts

// 案A: 配列化（シンプル）
interface KifuNode {
    // 既存のevalは後方互換性のため残す（multipv=1相当）
    eval?: KifuEval;
    // 複数PV用（multipv順、1-indexed相当）
    multiPvEvals?: KifuEval[];
}

// 案B: KifuEvalにmultipv追加
interface KifuEval {
    multipv?: number;  // 1-indexed (省略時は1)
    scoreCp?: number;
    scoreMate?: number;
    depth?: number;
    normalized?: boolean;
    pv?: string[];
}
```

**推奨: 案A**（既存コードへの影響が小さい）

#### 1.2 setNodeMultiPvEval関数の追加

```typescript
// packages/app-core/src/game/kifu-tree.ts
export function setNodeMultiPvEval(
    tree: KifuTree,
    nodeId: string,
    multipv: number,  // 1-indexed
    evalData: KifuEval
): KifuTree;
```

### Phase 2: 解析設定の拡張

#### 2.1 AnalysisSettingsにmultiPv追加

```typescript
// packages/ui/src/components/shogi-match/types.ts
interface AnalysisSettings {
    parallelWorkers: number;
    batchAnalysisTimeMs: number;
    batchAnalysisDepth: number;
    autoAnalyzeBranch: boolean;
    multiPv: number;  // 追加（デフォルト: 1）
}
```

#### 2.2 analyzePositionでMultiPV設定

```typescript
// packages/ui/src/components/shogi-match/hooks/useEngineManager.ts
// analyzePosition内で
await client.setOption("MultiPV", analysisSettings.multiPv);
```

### Phase 3: 評価値記録の拡張

#### 3.1 recordEvalByPlyの拡張

```typescript
// packages/ui/src/components/shogi-match/hooks/useKifuNavigation.ts
const recordEvalByPly = useCallback((ply: number, event: EngineInfoEvent) => {
    const multipv = event.multipv ?? 1;

    setTree((prev) => {
        const nodeId = findNodeByPlyInCurrentPath(prev, ply)
            ?? findNodeByPlyInMainLine(prev, ply);
        if (!nodeId) return prev;

        const evalData: KifuEval = {
            scoreCp: event.scoreCp,
            scoreMate: event.scoreMate,
            depth: event.depth,
            pv: event.pv,
        };

        // multipv=1 の場合は既存のevalも更新（後方互換性）
        if (multipv === 1) {
            return setNodeEval(
                setNodeMultiPvEval(prev, nodeId, multipv, evalData),
                nodeId,
                evalData
            );
        }
        return setNodeMultiPvEval(prev, nodeId, multipv, evalData);
    });
}, []);
```

### Phase 4: UI表示の拡張

#### 4.1 KifMoveの拡張

```typescript
// packages/ui/src/components/shogi-match/utils/kifFormat.ts
interface KifMove {
    ply: number;
    displayText: string;
    usiMove: string;
    // 既存フィールド（第1候補用、後方互換性）
    evalCp?: number;
    evalMate?: number;
    depth?: number;
    pv?: string[];
    // MultiPV用
    multiPvEvals?: Array<{
        multipv: number;
        evalCp?: number;
        evalMate?: number;
        depth?: number;
        pv?: string[];
    }>;
}
```

#### 4.2 ExpandedMoveDetailsの拡張

```tsx
// packages/ui/src/components/shogi-match/components/KifuPanel.tsx
function ExpandedMoveDetails({ move, ... }) {
    // 複数PVがある場合はタブまたはリストで表示
    const pvList = move.multiPvEvals ?? (move.pv ? [{
        multipv: 1,
        evalCp: move.evalCp,
        evalMate: move.evalMate,
        pv: move.pv,
    }] : []);

    return (
        <section>
            {pvList.map((pv, index) => (
                <div key={pv.multipv}>
                    <div>第{pv.multipv}候補: {formatEval(pv.evalCp, pv.evalMate)}</div>
                    <div>{convertPvToDisplay(pv.pv, position)}</div>
                    {/* 分岐作成ボタン等 */}
                </div>
            ))}
        </section>
    );
}
```

### Phase 5: 設定UIの追加

解析設定パネルにMultiPVスライダーを追加：

```tsx
// 解析設定セクション内
<label>
    候補手数 (MultiPV): {analysisSettings.multiPv}
    <input
        type="range"
        min={1}
        max={5}
        value={analysisSettings.multiPv}
        onChange={(e) => setAnalysisSettings({
            ...analysisSettings,
            multiPv: parseInt(e.target.value, 10)
        })}
    />
</label>
```

## 実装順序

1. **app-core**: `KifuNode.multiPvEvals`と`setNodeMultiPvEval`追加
2. **types.ts**: `AnalysisSettings.multiPv`追加
3. **useKifuNavigation**: `recordEvalByPly`でmultipv対応
4. **useEngineManager**: `analyzePosition`でMultiPV設定
5. **kifFormat.ts**: `KifMove.multiPvEvals`追加、変換ロジック
6. **KifuPanel.tsx**: 複数PV表示UI
7. 設定UI: MultiPVスライダー追加

## 注意点

- 後方互換性: `KifuNode.eval`（単一）は維持し、`multiPvEvals`を追加
- MultiPV=1の場合は従来動作と同じになるよう設計
- KIFインポート時は既存のeval形式を使用（multiPvEvalsは空）
- 一括解析でもMultiPV対応（並列ワーカー×MultiPVで負荷増加に注意）

## 関連ファイル

### 変更必要
- `packages/app-core/src/game/kifu-tree.ts`
- `packages/ui/src/components/shogi-match/types.ts`
- `packages/ui/src/components/shogi-match/hooks/useKifuNavigation.ts`
- `packages/ui/src/components/shogi-match/hooks/useEngineManager.ts`
- `packages/ui/src/components/shogi-match/utils/kifFormat.ts`
- `packages/ui/src/components/shogi-match/components/KifuPanel.tsx`

### 参考（既存実装）
- `packages/engine-client/src/index.ts` - EngineInfoEvent定義
- `apps/desktop/src-tauri/src/lib.rs` - MultiPVバックエンド処理
- `packages/rust-core/crates/engine-core/src/search/limits.rs` - multi_pv定義
