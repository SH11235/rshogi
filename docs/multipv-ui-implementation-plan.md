# MultiPV UI表示 実装計画

## 概要

検討モードでエンジン解析時に複数の候補手（読み筋）を表示できるようにする。
PC/スマホ両対応のUI設計。

## UI設計方針

### PC（KifuPanel）
- `ExpandedMoveDetails` をリスト形式で拡張
- 全候補を一覧表示し、評価値の比較がしやすい形式

### スマホ（MobileLayout）
- `MobileKifuBar` の手タップ → `BottomSheet` で詳細表示
- 既存レイアウトへの影響を最小化

---

## 実装ステップ

### Phase 1: データ構造の拡張

#### 1.1 KifuEval/KifuNode の拡張

**ファイル:** `packages/app-core/src/game/kifu-tree.ts`

```typescript
// KifuEval は変更なし（既にpvフィールドあり）

// KifuNode に multiPvEvals を追加
export interface KifuNode {
    // ... 既存フィールド ...

    /** 評価値情報（後方互換性のため残す、multipv=1相当） */
    eval?: KifuEval;

    /** 複数PV用の評価値配列（multipv順、1-indexed相当） */
    multiPvEvals?: KifuEval[];
}
```

#### 1.2 setNodeMultiPvEval 関数の追加

**ファイル:** `packages/app-core/src/game/kifu-tree.ts`

```typescript
/**
 * ノードに複数PVの評価値を設定
 * @param tree 棋譜ツリー
 * @param nodeId ノードID
 * @param multipv PV番号（1-indexed）
 * @param evalData 評価値データ
 * @returns 更新された棋譜ツリー
 */
export function setNodeMultiPvEval(
    tree: KifuTree,
    nodeId: string,
    multipv: number,
    evalData: KifuEval
): KifuTree;
```

---

### Phase 2: 解析設定の拡張

#### 2.1 AnalysisSettings に multiPv を追加

**ファイル:** `packages/ui/src/components/shogi-match/types.ts`

```typescript
export interface AnalysisSettings {
    parallelWorkers: number;
    batchAnalysisTimeMs: number;
    batchAnalysisDepth: number;
    autoAnalyzeBranch: boolean;
    /** 候補手数（MultiPV）、デフォルト: 1 */
    multiPv: number;
}

export const DEFAULT_ANALYSIS_SETTINGS: AnalysisSettings = {
    // ... 既存 ...
    multiPv: 1,
};
```

---

### Phase 3: 評価値記録の拡張

#### 3.1 recordEvalByPly の multiPV 対応

**ファイル:** `packages/ui/src/components/shogi-match/hooks/useKifuNavigation.ts`

```typescript
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
        let updated = setNodeMultiPvEval(prev, nodeId, multipv, evalData);
        if (multipv === 1) {
            updated = setNodeEval(updated, nodeId, evalData);
        }
        return updated;
    });
}, []);
```

#### 3.2 recordEvalByNodeId も同様に拡張

---

### Phase 4: エンジン解析で MultiPV 設定

#### 4.1 analyzePosition で MultiPV オプション設定

**ファイル:** `packages/ui/src/components/shogi-match/hooks/useEngineManager.ts`

```typescript
// analyzePosition 内で
await client.setOption("MultiPV", String(analysisSettings.multiPv));
```

---

### Phase 5: KifMove 型の拡張

#### 5.1 multiPvEvals フィールド追加

**ファイル:** `packages/ui/src/components/shogi-match/utils/kifFormat.ts`

```typescript
/** 単一PVの評価値情報 */
export interface PvEvalInfo {
    /** PV番号（1-indexed） */
    multipv: number;
    /** 評価値（センチポーン） */
    evalCp?: number;
    /** 詰み手数 */
    evalMate?: number;
    /** 探索深さ */
    depth?: number;
    /** 読み筋（USI形式） */
    pv?: string[];
}

export interface KifMove {
    // ... 既存フィールド ...

    /** 複数PV用の評価値配列 */
    multiPvEvals?: PvEvalInfo[];
}
```

#### 5.2 変換関数の拡張

`convertToKifMoves` 関数で `KifuNode.multiPvEvals` → `KifMove.multiPvEvals` の変換を追加。

---

### Phase 6: PC向け UI（KifuPanel）

#### 6.1 ExpandedMoveDetails の拡張

**ファイル:** `packages/ui/src/components/shogi-match/components/KifuPanel.tsx`

```tsx
function ExpandedMoveDetails({ move, position, ... }) {
    // 複数PVがある場合はリストで表示
    const pvList = move.multiPvEvals ?? (move.pv ? [{
        multipv: 1,
        evalCp: move.evalCp,
        evalMate: move.evalMate,
        depth: move.depth,
        pv: move.pv,
    }] : []);

    return (
        <section className="...">
            {/* ヘッダー */}
            <div className="flex items-center justify-between ...">
                <span>{move.ply}手目</span>
                <button onClick={onCollapse}>✕</button>
            </div>

            {/* 候補リスト */}
            {pvList.length > 0 ? (
                <div className="space-y-3">
                    {pvList.map((pv, index) => (
                        <PvCandidateItem
                            key={pv.multipv}
                            pv={pv}
                            position={position}
                            ply={move.ply}
                            onAddBranch={onAddBranch}
                            onPreview={onPreview}
                            isOnMainLine={isOnMainLine}
                            kifuTree={kifuTree}
                        />
                    ))}
                </div>
            ) : (
                /* 解析ボタン（PVがない場合） */
                <AnalyzeButton ... />
            )}
        </section>
    );
}
```

#### 6.2 PvCandidateItem コンポーネント（新規）

```tsx
function PvCandidateItem({
    pv,
    position,
    ply,
    onAddBranch,
    onPreview,
    isOnMainLine,
    kifuTree,
}: {
    pv: PvEvalInfo;
    position: PositionState;
    ply: number;
    onAddBranch?: (ply: number, pv: string[]) => void;
    onPreview?: (ply: number, pv: string[], evalCp?: number, evalMate?: number) => void;
    isOnMainLine: boolean;
    kifuTree?: KifuTree;
}) {
    const pvDisplay = useMemo(() => {
        if (!pv.pv || pv.pv.length === 0) return null;
        return convertPvToDisplay(pv.pv, position);
    }, [pv.pv, position]);

    const evalInfo = getEvalTooltipInfo(pv.evalCp, pv.evalMate, ply, pv.depth);

    return (
        <div className="border border-border rounded-lg p-2">
            {/* ヘッダー: 候補番号 + 評価値 */}
            <div className="flex items-center gap-2 mb-1">
                <span className="text-[11px] font-medium bg-muted px-1.5 py-0.5 rounded">
                    候補{pv.multipv}
                </span>
                <span className={`font-medium ${evalInfo.advantage === 'sente' ? 'text-wafuu-shu' : 'text-[hsl(210_70%_45%)]'}`}>
                    {formatEval(pv.evalCp, pv.evalMate, ply)}
                </span>
                {pv.depth && (
                    <span className="text-[10px] text-muted-foreground">
                        深さ{pv.depth}
                    </span>
                )}
            </div>

            {/* 読み筋 */}
            {pvDisplay && (
                <div className="flex flex-wrap gap-1 text-[12px] font-mono mb-2">
                    {pvDisplay.map((m, i) => (
                        <span key={i} className={m.turn === 'sente' ? 'text-wafuu-shu' : 'text-[hsl(210_70%_45%)]'}>
                            {m.displayText}
                            {i < pvDisplay.length - 1 && <span className="text-muted-foreground mx-0.5">→</span>}
                        </span>
                    ))}
                </div>
            )}

            {/* アクションボタン */}
            <div className="flex gap-2">
                {onPreview && pv.pv && (
                    <button onClick={() => onPreview(ply, pv.pv!, pv.evalCp, pv.evalMate)} className="...">
                        盤面で確認
                    </button>
                )}
                {onAddBranch && pv.pv && isOnMainLine && (
                    <button onClick={() => onAddBranch(ply, pv.pv!)} className="...">
                        分岐として保存
                    </button>
                )}
            </div>
        </div>
    );
}
```

---

### Phase 7: スマホ向け UI

#### 7.1 MoveDetailBottomSheet コンポーネント（新規）

**ファイル:** `packages/ui/src/components/shogi-match/components/MoveDetailBottomSheet.tsx`

```tsx
interface MoveDetailBottomSheetProps {
    isOpen: boolean;
    onClose: () => void;
    move: KifMove | null;
    position: PositionState | null;
    onAddBranch?: (ply: number, pv: string[]) => void;
    onPreview?: (ply: number, pv: string[], evalCp?: number, evalMate?: number) => void;
    isOnMainLine?: boolean;
    kifuTree?: KifuTree;
}

export function MoveDetailBottomSheet({
    isOpen,
    onClose,
    move,
    position,
    onAddBranch,
    onPreview,
    isOnMainLine = true,
    kifuTree,
}: MoveDetailBottomSheetProps) {
    if (!move || !position) return null;

    // 複数PVリスト
    const pvList = move.multiPvEvals ?? (move.pv ? [{
        multipv: 1,
        evalCp: move.evalCp,
        evalMate: move.evalMate,
        depth: move.depth,
        pv: move.pv,
    }] : []);

    return (
        <BottomSheet
            isOpen={isOpen}
            onClose={onClose}
            title={`${move.ply}手目の候補`}
            height="auto"
        >
            <div className="space-y-4">
                {/* 指し手表示 */}
                <div className="text-center">
                    <span className="text-lg font-medium">{move.displayText}</span>
                </div>

                {/* 候補リスト */}
                {pvList.length > 0 ? (
                    <div className="space-y-3">
                        {pvList.map((pv) => (
                            <MobilePvCandidateItem
                                key={pv.multipv}
                                pv={pv}
                                position={position}
                                ply={move.ply}
                                onAddBranch={onAddBranch}
                                onPreview={(ply, pvMoves, evalCp, evalMate) => {
                                    onClose(); // BottomSheetを閉じてからプレビュー
                                    onPreview?.(ply, pvMoves, evalCp, evalMate);
                                }}
                                isOnMainLine={isOnMainLine}
                            />
                        ))}
                    </div>
                ) : (
                    <div className="text-center text-muted-foreground py-4">
                        読み筋がありません
                    </div>
                )}
            </div>
        </BottomSheet>
    );
}
```

#### 7.2 MobilePvCandidateItem コンポーネント（新規）

PC版 `PvCandidateItem` のスマホ向けバージョン。タッチ操作に最適化したサイズ・余白。

#### 7.3 MobileLayout の拡張

**ファイル:** `packages/ui/src/components/shogi-match/layouts/MobileLayout.tsx`

```tsx
// Props に追加
interface MobileLayoutProps {
    // ... 既存 ...
    /** 選択された手の詳細表示用 */
    selectedMoveForDetail?: KifMove | null;
    onMoveDetailClose?: () => void;
    positionHistory?: PositionState[];
    onAddPvAsBranch?: (ply: number, pv: string[]) => void;
    onPreviewPv?: (ply: number, pv: string[], evalCp?: number, evalMate?: number) => void;
    kifuTree?: KifuTree;
    isOnMainLine?: boolean;
}

// コンポーネント内
const [selectedMoveDetail, setSelectedMoveDetail] = useState<KifMove | null>(null);

// MobileKifuBar の onPlySelect を拡張
const handlePlySelect = (ply: number) => {
    onPlySelect?.(ply);
    // 該当する手の詳細をセット
    const move = kifMoves?.find(m => m.ply === ply);
    if (move) {
        setSelectedMoveDetail(move);
    }
};

// BottomSheet を追加
<MoveDetailBottomSheet
    isOpen={selectedMoveDetail !== null}
    onClose={() => setSelectedMoveDetail(null)}
    move={selectedMoveDetail}
    position={selectedMoveDetail ? positionHistory?.[selectedMoveDetail.ply - 1] : null}
    onAddBranch={onAddPvAsBranch}
    onPreview={onPreviewPv}
    isOnMainLine={isOnMainLine}
    kifuTree={kifuTree}
/>
```

---

### Phase 8: 設定UIの追加

#### 8.1 BatchAnalysisDropdown に MultiPV スライダー追加

**ファイル:** `packages/ui/src/components/shogi-match/components/KifuPanel.tsx`

```tsx
// BatchAnalysisDropdown 内に追加
<div className="space-y-1.5">
    <div className="text-xs font-medium text-foreground">候補手数</div>
    <div className="flex gap-1 flex-wrap">
        {[1, 2, 3, 4, 5].map((n) => (
            <button
                key={n}
                type="button"
                onClick={() => onAnalysisSettingsChange({
                    ...analysisSettings,
                    multiPv: n,
                })}
                className={`px-2 py-1 rounded text-xs transition-colors ${
                    analysisSettings.multiPv === n
                        ? "bg-primary text-primary-foreground"
                        : "bg-muted text-muted-foreground hover:bg-muted/80"
                }`}
            >
                {n}
            </button>
        ))}
    </div>
</div>
```

---

## 実装順序（推奨）

1. **Phase 1**: app-core データ構造拡張
2. **Phase 2**: AnalysisSettings.multiPv 追加
3. **Phase 5**: KifMove 型拡張、変換関数
4. **Phase 3**: recordEvalByPly の multiPV 対応
5. **Phase 4**: エンジン解析で MultiPV 設定
6. **Phase 6**: PC向け UI（KifuPanel）
7. **Phase 7**: スマホ向け UI（BottomSheet）
8. **Phase 8**: 設定UIの MultiPV スライダー

---

## 注意点

- **後方互換性**: `KifuNode.eval`（単一）は維持し、`multiPvEvals` を追加
- **MultiPV=1の場合**: 従来動作と同じになるよう設計
- **KIFインポート時**: 既存のeval形式を使用（multiPvEvalsは空）
- **一括解析**: MultiPV × 並列ワーカーで負荷増加に注意
- **深さの扱い**: 各PVで深さが異なる可能性があるため、PvEvalInfo に depth を含める

---

## テストケース（データ層）

### 1. setNodeMultiPvEval 関数 (`kifu-tree.test.ts`)

```typescript
describe("setNodeMultiPvEval", () => {
    // 基本動作
    it("multipv=1 の評価値を設定できる", () => {});
    it("multipv=2 の評価値を設定できる", () => {});
    it("multipv=3 の評価値を設定できる", () => {});

    // 上書き動作
    it("同じmultipvの評価値を上書きできる", () => {});
    it("深さが深い評価値で上書きされる", () => {});

    // 配列の順序
    it("multiPvEvals は multipv 順にソートされる", () => {});
    it("multipv=2 を先に設定し、後から multipv=1 を設定しても順序が正しい", () => {});

    // エッジケース
    it("存在しないノードIDの場合はツリーを変更しない", () => {});
    it("空のevalDataでも設定できる", () => {});
});
```

### 2. recordEvalByPly の multiPV 対応 (`useKifuNavigation.test.ts`)

```typescript
describe("recordEvalByPly with multiPV", () => {
    // 基本動作
    it("multipv=1 のイベントで eval と multiPvEvals[0] の両方が更新される", () => {});
    it("multipv=2 のイベントで multiPvEvals[1] のみ更新される（evalは変更なし）", () => {});

    // 複数PVの記録
    it("同じplyに対して multipv=1,2,3 を順番に記録できる", () => {});
    it("multipv が省略された場合は 1 として扱う（後方互換性）", () => {});

    // 深さの扱い
    it("各multipvで異なる深さの評価値を保持できる", () => {});
});
```

### 3. KifMove 変換関数 (`kifFormat.test.ts`)

```typescript
describe("convertToKifMoves with multiPV", () => {
    // 基本変換
    it("KifuNode.multiPvEvals が KifMove.multiPvEvals に変換される", () => {});
    it("multiPvEvals が空の場合は undefined になる", () => {});

    // 後方互換性
    it("multiPvEvals がない場合でも eval から単一PVとして扱える", () => {});

    // PV情報の保持
    it("各PVの evalCp, evalMate, depth, pv が正しく変換される", () => {});
});
```

### 4. AnalysisSettings (`types.test.ts`)

```typescript
describe("AnalysisSettings", () => {
    it("DEFAULT_ANALYSIS_SETTINGS.multiPv は 1 である", () => {});
});
```

---

## 新規セッション用プロンプト

### データ層 + テスト（セッション1）

```
docs/multipv-ui-implementation-plan.md に従って MultiPV のデータ層を実装して

実装順序:
1. Phase 1: KifuNode.multiPvEvals と setNodeMultiPvEval 関数 + テスト
2. Phase 2: AnalysisSettings.multiPv 追加
3. Phase 5: KifMove.multiPvEvals と変換関数 + テスト
4. Phase 3: recordEvalByPly の multiPV 対応 + テスト
5. Phase 4: エンジン解析で MultiPV オプション設定

計画書の「テストケース（データ層）」セクションを参照してテストを実装
各Phaseでビルドとテストが通ることを確認しながら進めて
```

### UI層（セッション2）

```
docs/multipv-ui-implementation-plan.md に従って MultiPV の UI層を実装して

実装順序:
1. Phase 6: PC向け UI（KifuPanel の ExpandedMoveDetails 拡張、PvCandidateItem）
2. Phase 7: スマホ向け UI（MoveDetailBottomSheet 新規作成、MobileLayout 拡張）
3. Phase 8: 設定UIの MultiPV スライダー

各Phaseでビルドが通ることを確認しながら進めて
```

---

## 関連ファイル

### 変更対象
- `packages/app-core/src/game/kifu-tree.ts`
- `packages/ui/src/components/shogi-match/types.ts`
- `packages/ui/src/components/shogi-match/utils/kifFormat.ts`
- `packages/ui/src/components/shogi-match/hooks/useKifuNavigation.ts`
- `packages/ui/src/components/shogi-match/hooks/useEngineManager.ts`
- `packages/ui/src/components/shogi-match/components/KifuPanel.tsx`
- `packages/ui/src/components/shogi-match/layouts/MobileLayout.tsx`

### 新規作成
- `packages/ui/src/components/shogi-match/components/MoveDetailBottomSheet.tsx`

### 参考（既存実装）
- `packages/engine-client/src/index.ts` - EngineInfoEvent定義（multipvフィールドあり）
- `packages/ui/src/components/shogi-match/components/BottomSheet.tsx` - 既存BottomSheet
