import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { cn } from "@/lib/utils";
import type { Piece } from "shogi-core";
import { getPieceName } from "shogi-core";

interface PromotionDialogProps {
    piece: Piece;
    isOpen: boolean;
    onConfirm: (promote: boolean) => void;
    onCancel: () => void;
}

export function PromotionDialog({ piece, isOpen, onConfirm, onCancel }: PromotionDialogProps) {
    if (!isOpen) return null;

    const currentPieceName = getPieceName(piece, "ja");
    const promotedPiece = { ...piece, promoted: true };
    const promotedPieceName = getPieceName(promotedPiece, "ja");

    return (
        <div className="fixed inset-0 bg-black bg-opacity-50 flex items-center justify-center z-50">
            <Card className="w-80 max-w-sm mx-4">
                <CardHeader className="text-center">
                    <CardTitle className="text-lg">駒を成りますか？</CardTitle>
                </CardHeader>
                <CardContent className="space-y-4">
                    {/* 駒の表示 */}
                    <div className="flex justify-center gap-8">
                        {/* 現在の駒 */}
                        <div className="text-center">
                            <div
                                className={cn(
                                    "w-16 h-16 flex items-center justify-center text-2xl font-bold rounded border-2",
                                    "bg-white border-gray-300",
                                    piece.owner === "black" ? "text-black" : "text-red-600",
                                )}
                            >
                                {currentPieceName}
                            </div>
                            <p className="text-sm text-gray-600 mt-1">現在</p>
                        </div>

                        {/* 矢印 */}
                        <div className="flex items-center">
                            <span className="text-gray-400">→</span>
                        </div>

                        {/* 成った駒 */}
                        <div className="text-center">
                            <div
                                className={cn(
                                    "w-16 h-16 flex items-center justify-center text-2xl font-bold rounded border-2",
                                    "bg-yellow-100 border-yellow-300",
                                    piece.owner === "black" ? "text-black" : "text-red-600",
                                )}
                            >
                                {promotedPieceName}
                            </div>
                            <p className="text-sm text-gray-600 mt-1">成り</p>
                        </div>
                    </div>

                    {/* 選択ボタン */}
                    <div className="flex gap-2 justify-center">
                        <Button
                            type="button"
                            variant="outline"
                            onClick={() => onConfirm(false)}
                            className="flex-1"
                        >
                            成らない
                        </Button>
                        <Button
                            type="button"
                            onClick={() => onConfirm(true)}
                            className="flex-1 bg-yellow-500 hover:bg-yellow-600"
                        >
                            成る
                        </Button>
                    </div>

                    {/* キャンセルボタン */}
                    <Button
                        type="button"
                        variant="ghost"
                        onClick={onCancel}
                        className="w-full text-sm"
                    >
                        移動をキャンセル
                    </Button>
                </CardContent>
            </Card>
        </div>
    );
}
