/**
 * パスボタンコンポーネント
 *
 * パス手を実行するためのボタン。
 * - パス可能かどうかの状態に応じて有効/無効を切り替え
 * - 無効時はツールチップで理由を表示
 * - 確認ダイアログ（オプション）
 */

import { cn } from "@shogi/design-system";
import { type ReactElement, useState } from "react";
import {
    AlertDialog,
    AlertDialogAction,
    AlertDialogCancel,
    AlertDialogContent,
    AlertDialogDescription,
    AlertDialogFooter,
    AlertDialogHeader,
    AlertDialogTitle,
} from "../../alert-dialog";
import { Button } from "../../button";
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from "../../tooltip";

/** パス不可の理由 */
type PassDisabledReason =
    | "in-check" // 王手されている
    | "no-rights" // パス権がない
    | "not-your-turn" // 自分の手番ではない
    | "disabled" // パス権機能が無効
    | "match-not-running"; // 対局中でない

interface PassButtonProps {
    /** パス可能かどうか */
    canPass: boolean;
    /** パス不可の理由（ツールチップ表示用） */
    disabledReason?: PassDisabledReason;
    /** パスボタン押下時のコールバック */
    onPass: () => void;
    /** 確認ダイアログを表示するかどうか */
    showConfirmDialog?: boolean;
    /** 残りパス権数（確認ダイアログで表示） */
    remainingPassRights?: number;
    /** コンパクト表示（モバイル用） */
    compact?: boolean;
    /** 追加のクラス名 */
    className?: string;
}

/**
 * パス不可の理由をユーザー向けメッセージに変換
 */
function getDisabledReasonMessage(reason?: PassDisabledReason): string {
    switch (reason) {
        case "in-check":
            return "王手されているためパスできません";
        case "no-rights":
            return "パス権がありません";
        case "not-your-turn":
            return "あなたの手番ではありません";
        case "disabled":
            return "パス権機能が無効です";
        case "match-not-running":
            return "対局中ではありません";
        default:
            return "パスできません";
    }
}

/**
 * パスボタンコンポーネント
 */
export function PassButton({
    canPass,
    disabledReason,
    onPass,
    showConfirmDialog = false,
    remainingPassRights,
    compact = false,
    className,
}: PassButtonProps): ReactElement {
    const [isDialogOpen, setIsDialogOpen] = useState(false);

    const handleClick = () => {
        if (!canPass) return;
        if (showConfirmDialog) {
            setIsDialogOpen(true);
        } else {
            onPass();
        }
    };

    const handleConfirm = () => {
        setIsDialogOpen(false);
        onPass();
    };

    const button = (
        <Button
            variant={canPass ? "outline" : "ghost"}
            size={compact ? "sm" : "default"}
            disabled={!canPass}
            onClick={handleClick}
            className={cn("min-w-[4rem]", !canPass && "opacity-50 cursor-not-allowed", className)}
            aria-label={canPass ? "パスする" : getDisabledReasonMessage(disabledReason)}
        >
            パス
        </Button>
    );

    // パス不可の場合はツールチップで理由を表示
    const buttonWithTooltip = !canPass ? (
        <TooltipProvider>
            <Tooltip>
                <TooltipTrigger asChild>
                    <span className="inline-block">{button}</span>
                </TooltipTrigger>
                <TooltipContent>
                    <p>{getDisabledReasonMessage(disabledReason)}</p>
                </TooltipContent>
            </Tooltip>
        </TooltipProvider>
    ) : (
        button
    );

    return (
        <>
            {buttonWithTooltip}
            <AlertDialog open={isDialogOpen} onOpenChange={setIsDialogOpen}>
                <AlertDialogContent>
                    <AlertDialogHeader>
                        <AlertDialogTitle>パスしますか？</AlertDialogTitle>
                        <AlertDialogDescription>
                            {remainingPassRights !== undefined && (
                                <span className="block mb-2">
                                    パス権残り: {remainingPassRights}回
                                </span>
                            )}
                            パスすると手番が相手に移ります。
                        </AlertDialogDescription>
                    </AlertDialogHeader>
                    <AlertDialogFooter>
                        <AlertDialogCancel>キャンセル</AlertDialogCancel>
                        <AlertDialogAction onClick={handleConfirm}>パスする</AlertDialogAction>
                    </AlertDialogFooter>
                </AlertDialogContent>
            </AlertDialog>
        </>
    );
}
