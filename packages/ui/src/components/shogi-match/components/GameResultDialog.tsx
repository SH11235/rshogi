import type { GameResult } from "@shogi/app-core";
import { getReasonText, getWinnerLabel } from "@shogi/app-core";
import type { ReactElement } from "react";
import { Button } from "../../button";
import {
    Dialog,
    DialogContent,
    DialogDescription,
    DialogFooter,
    DialogHeader,
    DialogTitle,
} from "../../dialog";

interface GameResultDialogProps {
    result: GameResult | null;
    open: boolean;
    onClose: () => void;
}

export function GameResultDialog({
    result,
    open,
    onClose,
}: GameResultDialogProps): ReactElement | null {
    if (!result) {
        return null;
    }

    const winnerLabel = getWinnerLabel(result.winner);
    const reasonText = getReasonText(result.reason);

    return (
        <Dialog open={open} onOpenChange={(isOpen) => !isOpen && onClose()}>
            <DialogContent
                style={{
                    width: "min(400px, calc(100% - 32px))",
                    textAlign: "center",
                }}
            >
                <DialogHeader>
                    <DialogTitle
                        style={{
                            fontSize: "1.25rem",
                            textAlign: "center",
                        }}
                    >
                        対局終了
                    </DialogTitle>
                </DialogHeader>

                <div
                    style={{
                        display: "flex",
                        flexDirection: "column",
                        alignItems: "center",
                        gap: "16px",
                        padding: "16px 0",
                    }}
                >
                    <div
                        style={{
                            fontSize: "2rem",
                            fontWeight: "bold",
                            color: "hsl(var(--wafuu-kin, 42 85% 50%))",
                        }}
                    >
                        {winnerLabel}
                    </div>

                    <div
                        style={{
                            width: "100%",
                            height: "1px",
                            background: "hsl(var(--border, 0 0% 86%))",
                        }}
                    />

                    <DialogDescription
                        style={{
                            fontSize: "1rem",
                            color: "hsl(var(--foreground, 0 0% 10%))",
                        }}
                    >
                        {reasonText}
                    </DialogDescription>

                    <div
                        style={{
                            fontSize: "0.875rem",
                            color: "hsl(var(--muted-foreground, 0 0% 45%))",
                        }}
                    >
                        {result.totalMoves}手まで
                    </div>
                </div>

                <DialogFooter
                    style={{
                        justifyContent: "center",
                    }}
                >
                    <Button onClick={onClose} style={{ minWidth: "120px" }}>
                        閉じる
                    </Button>
                </DialogFooter>
            </DialogContent>
        </Dialog>
    );
}
