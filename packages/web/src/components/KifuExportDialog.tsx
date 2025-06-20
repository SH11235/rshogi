import { useCallback, useMemo, useState } from "react";
import { exportToKif, exportToSfen } from "shogi-core";
import type { Board, Hands, Move, Player } from "shogi-core";
import { Alert, AlertDescription } from "./ui/alert";
import { Button } from "./ui/button";
import {
    Dialog,
    DialogContent,
    DialogDescription,
    DialogFooter,
    DialogHeader,
    DialogTitle,
} from "./ui/dialog";
import { Input } from "./ui/input";
import { Label } from "./ui/label";
import { RadioGroup, RadioGroupItem } from "./ui/radio-group";
import { Textarea } from "./ui/textarea";

export type ExportFormat = "kif" | "sfen";

interface KifuExportDialogProps {
    open: boolean;
    onOpenChange: (open: boolean) => void;
    moveHistory: Move[];
    currentBoard: Board;
    currentHands: Hands;
    currentPlayer: Player;
    historyCursor: number;
}

export function KifuExportDialog({
    open,
    onOpenChange,
    moveHistory,
    currentBoard,
    currentHands,
    currentPlayer,
    historyCursor,
}: KifuExportDialogProps) {
    const [format, setFormat] = useState<ExportFormat>("kif");
    const [fileName, setFileName] = useState("");
    const [copied, setCopied] = useState(false);

    // エクスポート対象の手数を計算
    const exportMoves = useMemo(() => {
        if (historyCursor === -1 || historyCursor >= moveHistory.length - 1) {
            return moveHistory;
        }
        return moveHistory.slice(0, historyCursor + 1);
    }, [moveHistory, historyCursor]);

    // エクスポートコンテンツの生成
    const exportContent = useMemo(() => {
        try {
            if (format === "kif") {
                const gameInfo = {
                    開始日時: new Date().toLocaleString("ja-JP"),
                    先手: "先手",
                    後手: "後手",
                    棋戦: "自由対局",
                    手合割: "平手",
                };
                return exportToKif(exportMoves, gameInfo);
            }
            // SFEN形式の場合は現在の局面をエクスポート
            const moveNumber = historyCursor === -1 ? moveHistory.length + 1 : historyCursor + 2;
            return exportToSfen(currentBoard, currentHands, currentPlayer, moveNumber);
        } catch (error) {
            console.error("Export error:", error);
            return "";
        }
    }, [
        format,
        exportMoves,
        currentBoard,
        currentHands,
        currentPlayer,
        moveHistory.length,
        historyCursor,
    ]);

    // デフォルトのファイル名を生成
    const defaultFileName = useMemo(() => {
        const date = new Date().toISOString().split("T")[0];
        const extension = format === "kif" ? "kif" : "sfen";
        return `shogi_${date}.${extension}`;
    }, [format]);

    const handleCopy = useCallback(async () => {
        try {
            await navigator.clipboard.writeText(exportContent);
            setCopied(true);
            setTimeout(() => setCopied(false), 2000);
        } catch (error) {
            console.error("Copy failed:", error);
        }
    }, [exportContent]);

    const handleDownload = useCallback(() => {
        const blob = new Blob([exportContent], { type: "text/plain;charset=utf-8" });
        const url = window.URL.createObjectURL(blob);
        const link = document.createElement("a");
        link.href = url;
        link.download = fileName || defaultFileName;
        document.body.appendChild(link);
        link.click();
        document.body.removeChild(link);
        window.URL.revokeObjectURL(url);
        onOpenChange(false);
    }, [exportContent, fileName, defaultFileName, onOpenChange]);

    const handleClose = useCallback(() => {
        onOpenChange(false);
        setFormat("kif");
        setFileName("");
        setCopied(false);
    }, [onOpenChange]);

    return (
        <Dialog open={open} onOpenChange={handleClose}>
            <DialogContent className="sm:max-w-[600px]">
                <DialogHeader>
                    <DialogTitle>棋譜をエクスポート</DialogTitle>
                    <DialogDescription>
                        {format === "kif"
                            ? "KIF形式で棋譜をエクスポートします"
                            : "現在の局面をSFEN形式でエクスポートします"}
                        {historyCursor !== -1 && historyCursor < moveHistory.length - 1 && (
                            <span className="block mt-1 text-warning">
                                ※ 現在表示中の手（第{historyCursor + 1}手）までをエクスポートします
                            </span>
                        )}
                    </DialogDescription>
                </DialogHeader>

                <div className="space-y-4">
                    <div>
                        <Label htmlFor="format">エクスポート形式</Label>
                        <RadioGroup
                            id="format"
                            value={format}
                            onValueChange={(value) => setFormat(value as ExportFormat)}
                            className="flex gap-4 mt-2"
                        >
                            <div className="flex items-center space-x-2">
                                <RadioGroupItem value="kif" id="kif-export" />
                                <Label htmlFor="kif-export" className="font-normal cursor-pointer">
                                    KIF形式（棋譜）
                                </Label>
                            </div>
                            <div className="flex items-center space-x-2">
                                <RadioGroupItem value="sfen" id="sfen-export" />
                                <Label htmlFor="sfen-export" className="font-normal cursor-pointer">
                                    SFEN形式（局面）
                                </Label>
                            </div>
                        </RadioGroup>
                    </div>

                    <div>
                        <Label htmlFor="preview">プレビュー</Label>
                        <Textarea
                            id="preview"
                            value={exportContent}
                            readOnly
                            className="min-h-[200px] font-mono text-sm resize-none"
                        />
                        <div className="flex items-center justify-between mt-2">
                            <p className="text-sm text-muted-foreground">
                                {format === "kif" ? `${exportMoves.length}手の棋譜` : "現在の局面"}
                            </p>
                            <Button type="button" variant="outline" size="sm" onClick={handleCopy}>
                                {copied ? "コピーしました！" : "クリップボードにコピー"}
                            </Button>
                        </div>
                    </div>

                    <div>
                        <Label htmlFor="filename">ファイル名（オプション）</Label>
                        <Input
                            id="filename"
                            value={fileName}
                            onChange={(e: React.ChangeEvent<HTMLInputElement>) =>
                                setFileName(e.target.value)
                            }
                            placeholder={defaultFileName}
                            className="mt-1"
                        />
                    </div>

                    {format === "kif" && exportMoves.length === 0 && (
                        <Alert variant="destructive">
                            <AlertDescription>エクスポートする手がありません</AlertDescription>
                        </Alert>
                    )}
                </div>

                <DialogFooter>
                    <Button variant="outline" onClick={handleClose}>
                        キャンセル
                    </Button>
                    <Button
                        onClick={handleDownload}
                        disabled={format === "kif" && exportMoves.length === 0}
                    >
                        ダウンロード
                    </Button>
                </DialogFooter>
            </DialogContent>
        </Dialog>
    );
}
