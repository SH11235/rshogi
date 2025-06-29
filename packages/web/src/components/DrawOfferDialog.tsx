import {
    AlertDialog,
    AlertDialogAction,
    AlertDialogCancel,
    AlertDialogContent,
    AlertDialogDescription,
    AlertDialogFooter,
    AlertDialogHeader,
    AlertDialogTitle,
} from "@/components/ui/alert-dialog";
import { Button } from "@/components/ui/button";
import { useGameStore } from "@/stores/gameStore";

export function DrawOfferDialog() {
    const {
        drawOfferPending,
        pendingDrawOfferer,
        localPlayer,
        currentPlayer,
        acceptDrawOffer,
        rejectDrawOffer,
        offerDraw,
        gameStatus,
        isOnlineGame,
    } = useGameStore();

    // 自分が提案を受けた場合のダイアログを表示
    const showReceiveDialog =
        drawOfferPending && pendingDrawOfferer !== localPlayer && localPlayer !== null;

    // ローカル対戦で相手の手番に提案を受けた場合
    const showLocalReceiveDialog =
        drawOfferPending && !isOnlineGame && pendingDrawOfferer !== currentPlayer;

    // 引き分け提案ボタンを表示する条件
    const canOfferDraw =
        (gameStatus === "playing" || gameStatus === "check") &&
        (!isOnlineGame || (isOnlineGame && localPlayer === currentPlayer));

    if (showReceiveDialog || showLocalReceiveDialog) {
        const offererName = pendingDrawOfferer === "black" ? "先手" : "後手";

        return (
            <AlertDialog open={true}>
                <AlertDialogContent>
                    <AlertDialogHeader>
                        <AlertDialogTitle>引き分け提案</AlertDialogTitle>
                        <AlertDialogDescription>
                            {offererName}から引き分けの提案がありました。承認しますか？
                        </AlertDialogDescription>
                    </AlertDialogHeader>
                    <AlertDialogFooter>
                        <AlertDialogCancel onClick={rejectDrawOffer}>拒否</AlertDialogCancel>
                        <AlertDialogAction onClick={acceptDrawOffer}>承認</AlertDialogAction>
                    </AlertDialogFooter>
                </AlertDialogContent>
            </AlertDialog>
        );
    }

    // 引き分け提案ボタン
    return canOfferDraw ? (
        <Button onClick={offerDraw} variant="outline" disabled={drawOfferPending}>
            {drawOfferPending ? "引き分け提案中..." : "引き分け提案"}
        </Button>
    ) : null;
}
