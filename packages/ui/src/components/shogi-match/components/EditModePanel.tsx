import type { ReactElement } from "react";
import { Button } from "../../button";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "../../collapsible";

interface EditModePanelProps {
    // パネル表示状態
    isOpen: boolean;
    onOpenChange: (open: boolean) => void;

    // アクション
    onResetToStartpos: () => Promise<void>;
    onClearBoard: () => void;

    // 制約
    isMatchRunning: boolean;
    positionReady: boolean;

    // メッセージ
    message: string | null;
}

export function EditModePanel({
    isOpen,
    onOpenChange,
    onResetToStartpos,
    onClearBoard,
    isMatchRunning,
    positionReady,
    message,
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
                    width: "var(--panel-width)",
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
                            borderTop: "none",
                            borderLeft: "none",
                            borderRight: "none",
                            borderBottom: isOpen ? "1px solid hsl(var(--wafuu-border))" : "none",
                            display: "flex",
                            alignItems: "center",
                            justifyContent: "space-between",
                            cursor: "pointer",
                            transition: "all 0.2s ease",
                        }}
                    >
                        <span
                            style={{
                                fontSize: "18px",
                                fontWeight: 700,
                                color: isOpen ? "hsl(var(--wafuu-shu))" : "hsl(var(--wafuu-sumi))",
                                letterSpacing: "0.05em",
                                transition: "color 0.2s ease",
                            }}
                        >
                            {isOpen ? "局面編集中" : "局面編集"}
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
                            駒をドラッグして盤面を編集できます。持ち駒の±ボタンで駒数を調整できます。
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
                        {message && (
                            <div
                                style={{
                                    fontSize: "13px",
                                    color: "hsl(var(--wafuu-shu))",
                                    padding: "10px",
                                    background: "hsl(var(--wafuu-washi))",
                                    borderRadius: "8px",
                                    borderLeft: "3px solid hsl(var(--wafuu-shu))",
                                }}
                            >
                                {message}
                            </div>
                        )}
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
                                    <strong>駒を配置:</strong> 持ち駒から盤面にドラッグ
                                </li>
                                <li>
                                    <strong>駒を移動:</strong> 盤面の駒をドラッグして移動
                                </li>
                                <li>
                                    <strong>駒を削除:</strong> 盤外にドラッグ
                                </li>
                            </ul>
                        </div>
                    </div>
                </CollapsibleContent>
            </div>
        </Collapsible>
    );
}
