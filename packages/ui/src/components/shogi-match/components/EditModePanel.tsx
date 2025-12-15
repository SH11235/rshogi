import type { PieceType, Player, Square } from "@shogi/app-core";
import type { ReactElement } from "react";
import { Button } from "../../button";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "../../collapsible";
import { isPromotable, PIECE_LABELS } from "../utils/constants";

const PIECE_SELECT_ORDER: PieceType[] = ["K", "R", "B", "G", "S", "N", "L", "P"];

export interface EditModePanelProps {
    // パネル表示状態
    isOpen: boolean;
    onOpenChange: (open: boolean) => void;

    // 編集状態
    editOwner: Player;
    editPieceType: PieceType | null;
    editPromoted: boolean;
    editFromSquare: Square | null;
    editTool: "place" | "erase";

    // 状態更新関数
    setEditOwner: (owner: Player) => void;
    setEditPieceType: (type: PieceType | null) => void;
    setEditPromoted: (promoted: boolean) => void;
    setEditTool: (tool: "place" | "erase") => void;

    // アクション
    onResetToStartpos: () => Promise<void>;
    onClearBoard: () => void;

    // 制約
    isMatchRunning: boolean;
    positionReady: boolean;
}

export function EditModePanel({
    isOpen,
    onOpenChange,
    editOwner,
    editPieceType,
    editPromoted,
    editFromSquare,
    editTool,
    setEditOwner,
    setEditPieceType,
    setEditPromoted,
    setEditTool,
    onResetToStartpos,
    onClearBoard,
    isMatchRunning,
    positionReady,
}: EditModePanelProps): ReactElement {
    return (
        <Collapsible open={isOpen} onOpenChange={onOpenChange}>
            <div
                style={{
                    background: "hsl(var(--wafuu-washi-warm))",
                    border: "2px solid hsl(var(--wafuu-border))",
                    borderRadius: "12px",
                    overflow: "hidden",
                    boxShadow: "0 8px 20px rgba(0,0,0,0.08)",
                }}
            >
                <CollapsibleTrigger asChild>
                    <button
                        type="button"
                        aria-label="局面編集パネルを開閉"
                        style={{
                            width: "100%",
                            padding: "14px 16px",
                            background:
                                "linear-gradient(135deg, hsl(var(--wafuu-washi)) 0%, hsl(var(--wafuu-washi-warm)) 100%)",
                            borderBottom: isOpen ? "1px solid hsl(var(--wafuu-border))" : "none",
                            display: "flex",
                            alignItems: "center",
                            justifyContent: "space-between",
                            cursor: "pointer",
                            transition: "all 0.2s ease",
                            border: "none",
                        }}
                    >
                        <span
                            style={{
                                fontSize: "18px",
                                fontWeight: 700,
                                color: "hsl(var(--wafuu-sumi))",
                                letterSpacing: "0.05em",
                            }}
                        >
                            局面編集
                        </span>
                        <span
                            style={{
                                fontSize: "20px",
                                color: "hsl(var(--wafuu-kincha))",
                                transform: isOpen ? "rotate(180deg)" : "rotate(0deg)",
                                transition: "transform 0.2s ease",
                            }}
                        >
                            ▼
                        </span>
                    </button>
                </CollapsibleTrigger>
                <CollapsibleContent>
                    <div
                        style={{
                            padding: "16px",
                            display: "flex",
                            flexDirection: "column",
                            gap: "14px",
                        }}
                    >
                        <div
                            style={{
                                fontSize: "12px",
                                color: "hsl(var(--wafuu-sumi-light))",
                                padding: "10px",
                                background: "hsl(var(--wafuu-washi))",
                                borderRadius: "8px",
                                borderLeft: "3px solid hsl(var(--wafuu-kin))",
                            }}
                        >
                            盤面をクリックして局面を編集できます。対局開始前のみ有効です。王は重複不可、各駒は上限枚数まで配置できます。
                        </div>
                        <div
                            style={{
                                display: "flex",
                                gap: "8px",
                                flexWrap: "wrap",
                            }}
                        >
                            <Button
                                type="button"
                                onClick={onResetToStartpos}
                                disabled={isMatchRunning || !positionReady}
                                variant="outline"
                                style={{ paddingInline: "12px" }}
                            >
                                平手に戻す
                            </Button>
                            <Button
                                type="button"
                                onClick={onClearBoard}
                                disabled={isMatchRunning || !positionReady}
                                variant="outline"
                                style={{ paddingInline: "12px" }}
                            >
                                盤面をクリア
                            </Button>
                        </div>
                        <div
                            style={{
                                display: "flex",
                                gap: "6px",
                                alignItems: "center",
                            }}
                        >
                            <span
                                style={{
                                    fontSize: "12px",
                                    color: "hsl(var(--muted-foreground, 0 0% 48%))",
                                }}
                            >
                                配置する先後
                            </span>
                            <label
                                style={{
                                    display: "flex",
                                    gap: "4px",
                                    fontSize: "13px",
                                }}
                            >
                                <input
                                    type="radio"
                                    name="edit-owner"
                                    value="sente"
                                    checked={editOwner === "sente"}
                                    disabled={isMatchRunning}
                                    onChange={() => setEditOwner("sente")}
                                />
                                先手
                            </label>
                            <label
                                style={{
                                    display: "flex",
                                    gap: "4px",
                                    fontSize: "13px",
                                }}
                            >
                                <input
                                    type="radio"
                                    name="edit-owner"
                                    value="gote"
                                    checked={editOwner === "gote"}
                                    disabled={isMatchRunning}
                                    onChange={() => setEditOwner("gote")}
                                />
                                後手
                            </label>
                        </div>
                        <div
                            style={{
                                display: "flex",
                                gap: "8px",
                                flexWrap: "wrap",
                                alignItems: "center",
                            }}
                        >
                            <div
                                style={{
                                    display: "flex",
                                    gap: "6px",
                                    flexWrap: "wrap",
                                }}
                            >
                                {PIECE_SELECT_ORDER.map((type) => {
                                    const selected = editPieceType === type && editTool === "place";
                                    return (
                                        <Button
                                            key={type}
                                            type="button"
                                            variant={selected ? "secondary" : "outline"}
                                            onClick={() => {
                                                if (selected) {
                                                    // 選択中の駒を再度クリック：選択解除
                                                    setEditPieceType(null);
                                                } else {
                                                    setEditTool("place");
                                                    setEditPieceType(type);
                                                    if (!isPromotable(type)) {
                                                        setEditPromoted(false);
                                                    }
                                                }
                                            }}
                                            disabled={isMatchRunning}
                                            style={{ paddingInline: "10px" }}
                                        >
                                            {PIECE_LABELS[type]}
                                        </Button>
                                    );
                                })}
                            </div>
                            <Button
                                type="button"
                                variant={editTool === "erase" ? "secondary" : "outline"}
                                onClick={() => {
                                    if (editTool === "erase") {
                                        // 削除モードを解除
                                        setEditTool("place");
                                    } else {
                                        // 削除モードに切り替え
                                        setEditTool("erase");
                                        setEditPieceType(null);
                                    }
                                }}
                                disabled={isMatchRunning}
                                style={{ paddingInline: "10px" }}
                            >
                                削除モード
                            </Button>
                            <label
                                style={{
                                    display: "flex",
                                    alignItems: "center",
                                    gap: "6px",
                                    fontSize: "13px",
                                }}
                            >
                                <input
                                    type="checkbox"
                                    checked={editPromoted}
                                    disabled={
                                        isMatchRunning ||
                                        !editPieceType ||
                                        !isPromotable(editPieceType)
                                    }
                                    onChange={(e) => setEditPromoted(e.target.checked)}
                                />
                                成りで配置
                            </label>
                        </div>
                        <div
                            style={{
                                fontSize: "12px",
                                color: "hsl(var(--wafuu-sumi-light))",
                                padding: "12px",
                                background: "hsl(var(--wafuu-washi))",
                                borderRadius: "8px",
                                borderLeft: "3px solid hsl(var(--wafuu-shu))",
                            }}
                        >
                            <div
                                style={{
                                    fontWeight: 600,
                                    marginBottom: "6px",
                                    color: "hsl(var(--wafuu-sumi))",
                                }}
                            >
                                操作方法
                            </div>
                            <ul
                                style={{
                                    margin: 0,
                                    paddingLeft: "20px",
                                    lineHeight: 1.6,
                                }}
                            >
                                <li>
                                    <strong>駒を配置:</strong> 駒ボタンを選択 → 盤面をクリック
                                </li>
                                <li>
                                    <strong>駒を移動:</strong>{" "}
                                    駒ボタン未選択の状態で盤面の駒をクリック → 移動先をクリック
                                </li>
                                <li>
                                    <strong>駒を削除:</strong>{" "}
                                    削除モードボタンを押して盤面をクリック（手駒に戻ります）
                                </li>
                                <li>
                                    <strong>選択解除:</strong>{" "}
                                    駒ボタンや削除モードボタンを再度クリック、または同じマスを再度クリック
                                </li>
                            </ul>
                            {editFromSquare && (
                                <div
                                    style={{
                                        marginTop: "8px",
                                        padding: "6px 10px",
                                        background: "hsl(var(--wafuu-kin) / 0.15)",
                                        borderRadius: "6px",
                                        color: "hsl(var(--wafuu-sumi))",
                                        fontSize: "11px",
                                        fontWeight: 600,
                                    }}
                                >
                                    移動元: {editFromSquare} → 移動先を選択してください
                                </div>
                            )}
                        </div>
                    </div>
                </CollapsibleContent>
            </div>
        </Collapsible>
    );
}
