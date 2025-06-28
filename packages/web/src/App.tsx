import "./App.css";
import { Wifi } from "lucide-react";
import { useState } from "react";
import type { Move } from "shogi-core";
import { AudioTestPanel } from "./components/AudioTestPanel";
import { Board } from "./components/Board";
import { CapturedPieces } from "./components/CapturedPieces";
import { ConnectionStatusComponent } from "./components/ConnectionStatus";
import { GameControls } from "./components/GameControls";
import { GameInfo } from "./components/GameInfo";
import { KifuExportDialog } from "./components/KifuExportDialog";
import { KifuImportDialog } from "./components/KifuImportDialog";
import type { ImportFormat } from "./components/KifuImportDialog";
import { MoveHistory } from "./components/MoveHistory";
import { OnlineGameDialog } from "./components/OnlineGameDialog";
import { PlaybackControls } from "./components/PlaybackControls";
import { PromotionDialog } from "./components/PromotionDialog";
import { Button } from "./components/ui/button";
import { useKeyboardShortcuts } from "./hooks/useKeyboardShortcuts";
import { useGameStore } from "./stores/gameStore";

function App() {
    const {
        board,
        hands,
        currentPlayer,
        selectedSquare,
        selectedDropPiece,
        validMoves,
        validDropSquares,
        gameStatus,
        moveHistory,
        historyCursor,
        promotionPending,
        resignedPlayer,
        branchInfo,
        isTsumeShogi,
        gameMode,
        selectSquare,
        selectDropPiece,
        confirmPromotion,
        cancelPromotion,
        clearSelections,
        resetGame,
        resign,
        importGame,
        importSfen,
        undo,
        redo,
        goToMove,
        canUndo,
        canRedo,
        navigateNext,
        navigatePrevious,
        navigateFirst,
        navigateLast,
        returnToMainLine,
        isInBranch,
        canNavigateNext,
        canNavigatePrevious,
        startGameFromPosition,
        returnToReviewMode,
        reviewBasePosition,
        gameUndo,
        gameRedo,
        canGameUndo,
        canGameRedo,
        // 通信対戦関連
        isOnlineGame,
        connectionStatus,
        startOnlineGame,
        joinOnlineGame,
        acceptOnlineAnswer,
        // disconnectOnline, // 将来使用予定
    } = useGameStore();

    const [isImportDialogOpen, setIsImportDialogOpen] = useState(false);
    const [isExportDialogOpen, setIsExportDialogOpen] = useState(false);
    const [isOnlineDialogOpen, setIsOnlineDialogOpen] = useState(false);

    // キーボードショートカットの設定
    useKeyboardShortcuts({
        onUndo:
            gameMode === "playing"
                ? canGameUndo()
                    ? gameUndo
                    : undefined
                : canUndo()
                  ? undo
                  : undefined,
        onRedo:
            gameMode === "playing"
                ? canGameRedo()
                    ? gameRedo
                    : undefined
                : canRedo()
                  ? redo
                  : undefined,
        onReset: resetGame,
        onEscape: () => {
            // プロモーションダイアログをキャンセル
            if (promotionPending) {
                cancelPromotion();
            } else {
                // 選択状態をクリア
                clearSelections();
            }
        },
        onEnter: promotionPending ? () => confirmPromotion(true) : undefined,
        onNavigateNext: gameMode !== "playing" && canNavigateNext() ? navigateNext : undefined,
        onNavigatePrevious:
            gameMode !== "playing" && canNavigatePrevious() ? navigatePrevious : undefined,
        onNavigateFirst: gameMode !== "playing" ? navigateFirst : undefined,
        onNavigateLast: gameMode !== "playing" ? navigateLast : undefined,
        onReturnToMainLine: gameMode !== "playing" && isInBranch() ? returnToMainLine : undefined,
    });

    const handleImport = (moves: Move[], format: ImportFormat, content?: string) => {
        if (format === "kif") {
            importGame(moves, content);
        } else if (format === "sfen" && content) {
            importSfen(content);
        }
    };

    return (
        <div className="min-h-screen bg-gray-100 py-4 sm:py-8">
            <div className="container mx-auto px-2 sm:px-4">
                <div className="flex flex-col sm:flex-row items-center justify-center gap-4 mb-4 sm:mb-8">
                    <h1 className="text-2xl sm:text-3xl lg:text-4xl font-bold">将棋</h1>
                    <div className="flex gap-2">
                        <Button
                            variant="outline"
                            size="sm"
                            onClick={() => setIsOnlineDialogOpen(true)}
                            disabled={isOnlineGame}
                        >
                            <Wifi className="mr-2 h-4 w-4" />
                            通信対戦
                        </Button>
                        <Button
                            variant="outline"
                            size="sm"
                            onClick={() => setIsImportDialogOpen(true)}
                            disabled={isOnlineGame}
                        >
                            📤 棋譜読込
                        </Button>
                        <Button
                            variant="outline"
                            size="sm"
                            onClick={() => setIsExportDialogOpen(true)}
                            disabled={moveHistory.length === 0}
                        >
                            📥 棋譜保存
                        </Button>
                    </div>
                </div>

                {/* 通信対戦の接続状態表示 */}
                {isOnlineGame && (
                    <div className="flex justify-center mb-4">
                        <ConnectionStatusComponent
                            status={connectionStatus}
                            onReconnect={() => setIsOnlineDialogOpen(true)}
                        />
                    </div>
                )}

                {/* モバイル向け: 縦配置メインレイアウト */}
                <div className="flex flex-col lg:hidden space-y-4">
                    {/* 後手の持ち駒（上部） */}
                    <div className="w-full">
                        <CapturedPieces
                            hands={hands}
                            player="white"
                            currentPlayer={currentPlayer}
                            selectedDropPiece={selectedDropPiece}
                            onPieceClick={selectDropPiece}
                        />
                    </div>

                    {/* 将棋盤とゲーム情報を並べる */}
                    <div className="flex flex-col sm:flex-row gap-4">
                        <div className="flex-1 flex justify-center pr-6 sm:pr-8">
                            <Board
                                board={board}
                                selectedSquare={selectedSquare}
                                validMoves={validMoves}
                                validDropSquares={validDropSquares}
                                onSquareClick={selectSquare}
                            />
                        </div>
                        <div className="flex-shrink-0 w-80 sm:w-96 mx-auto sm:mx-0">
                            <div className="space-y-4">
                                <GameInfo
                                    currentPlayer={currentPlayer}
                                    gameStatus={gameStatus}
                                    moveHistory={moveHistory}
                                    historyCursor={historyCursor}
                                    resignedPlayer={resignedPlayer}
                                    isTsumeShogi={isTsumeShogi}
                                    gameMode={gameMode}
                                    hasReviewBase={!!reviewBasePosition}
                                    onReset={resetGame}
                                    onResign={resign}
                                    onStartFromPosition={startGameFromPosition}
                                    onReturnToReview={returnToReviewMode}
                                />
                                {gameMode === "playing" ? (
                                    <GameControls
                                        canUndo={canGameUndo()}
                                        canRedo={canGameRedo()}
                                        onUndo={gameUndo}
                                        onRedo={gameRedo}
                                    />
                                ) : (
                                    <PlaybackControls
                                        canNavigateNext={canNavigateNext()}
                                        canNavigatePrevious={canNavigatePrevious()}
                                        isInBranch={isInBranch()}
                                        onNavigateFirst={navigateFirst}
                                        onNavigatePrevious={navigatePrevious}
                                        onNavigateNext={navigateNext}
                                        onNavigateLast={navigateLast}
                                        onReturnToMainLine={returnToMainLine}
                                    />
                                )}
                                <MoveHistory
                                    moveHistory={moveHistory}
                                    historyCursor={historyCursor}
                                    isInBranch={isInBranch()}
                                    branchPoint={branchInfo?.branchPoint}
                                    onGoToMove={goToMove}
                                />
                            </div>
                        </div>
                    </div>

                    {/* 先手の持ち駒（下部） */}
                    <div className="w-full">
                        <CapturedPieces
                            hands={hands}
                            player="black"
                            currentPlayer={currentPlayer}
                            selectedDropPiece={selectedDropPiece}
                            onPieceClick={selectDropPiece}
                        />
                    </div>
                </div>

                {/* デスクトップ向け: 横配置レイアウト */}
                <div className="hidden lg:flex gap-6 xl:gap-8 justify-center items-start max-w-7xl mx-auto">
                    {/* 左側パネル（ゲーム情報・コントロール） */}
                    <div className="flex-shrink-0 w-64 xl:w-80 space-y-4">
                        <GameInfo
                            currentPlayer={currentPlayer}
                            gameStatus={gameStatus}
                            moveHistory={moveHistory}
                            historyCursor={historyCursor}
                            resignedPlayer={resignedPlayer}
                            isTsumeShogi={isTsumeShogi}
                            gameMode={gameMode}
                            hasReviewBase={!!reviewBasePosition}
                            onReset={resetGame}
                            onResign={resign}
                            onStartFromPosition={startGameFromPosition}
                            onReturnToReview={returnToReviewMode}
                        />
                        {gameMode === "playing" ? (
                            <GameControls
                                canUndo={canGameUndo()}
                                canRedo={canGameRedo()}
                                onUndo={gameUndo}
                                onRedo={gameRedo}
                            />
                        ) : (
                            <PlaybackControls
                                canNavigateNext={canNavigateNext()}
                                canNavigatePrevious={canNavigatePrevious()}
                                isInBranch={isInBranch()}
                                onNavigateFirst={navigateFirst}
                                onNavigatePrevious={navigatePrevious}
                                onNavigateNext={navigateNext}
                                onNavigateLast={navigateLast}
                                onReturnToMainLine={returnToMainLine}
                            />
                        )}
                    </div>

                    {/* 中央：将棋盤と持ち駒 */}
                    <div className="flex-shrink-0 space-y-4">
                        {/* 後手の持ち駒 */}
                        <div className="w-full max-w-md mx-auto">
                            <CapturedPieces
                                hands={hands}
                                player="white"
                                currentPlayer={currentPlayer}
                                selectedDropPiece={selectedDropPiece}
                                onPieceClick={selectDropPiece}
                            />
                        </div>

                        {/* 将棋盤 */}
                        <div className="pr-6 xl:pr-8">
                            <Board
                                board={board}
                                selectedSquare={selectedSquare}
                                validMoves={validMoves}
                                validDropSquares={validDropSquares}
                                onSquareClick={selectSquare}
                            />
                        </div>

                        {/* 先手の持ち駒 */}
                        <div className="w-full max-w-md mx-auto">
                            <CapturedPieces
                                hands={hands}
                                player="black"
                                currentPlayer={currentPlayer}
                                selectedDropPiece={selectedDropPiece}
                                onPieceClick={selectDropPiece}
                            />
                        </div>
                    </div>

                    {/* 右側パネル（棋譜） */}
                    <div className="flex-shrink-0 w-64 xl:w-80">
                        <MoveHistory
                            moveHistory={moveHistory}
                            historyCursor={historyCursor}
                            isInBranch={isInBranch()}
                            branchPoint={branchInfo?.branchPoint}
                            onGoToMove={goToMove}
                        />
                    </div>
                </div>

                {/* プロモーションダイアログ */}
                {promotionPending && (
                    <PromotionDialog
                        piece={promotionPending.piece}
                        isOpen={true}
                        onConfirm={confirmPromotion}
                        onCancel={cancelPromotion}
                    />
                )}

                {/* 開発用音声テストパネル */}
                <div className="mt-8">
                    <AudioTestPanel />
                </div>

                {/* 棋譜インポート・エクスポートダイアログ */}
                <KifuImportDialog
                    open={isImportDialogOpen}
                    onOpenChange={setIsImportDialogOpen}
                    onImport={handleImport}
                />
                <KifuExportDialog
                    open={isExportDialogOpen}
                    onOpenChange={setIsExportDialogOpen}
                    moveHistory={moveHistory}
                    currentBoard={board}
                    currentHands={hands}
                    currentPlayer={currentPlayer}
                    historyCursor={historyCursor}
                />

                {/* 通信対戦ダイアログ */}
                <OnlineGameDialog
                    open={isOnlineDialogOpen}
                    onOpenChange={setIsOnlineDialogOpen}
                    onCreateHost={() => startOnlineGame(true)}
                    onJoinAsGuest={joinOnlineGame}
                    onAcceptAnswer={acceptOnlineAnswer}
                />
            </div>
        </div>
    );
}

export default App;
