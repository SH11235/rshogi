import "./App.css";
import { Board } from "./components/Board";
import { CapturedPieces } from "./components/CapturedPieces";
import { GameInfo } from "./components/GameInfo";
import { MoveHistory } from "./components/MoveHistory";
import { PromotionDialog } from "./components/PromotionDialog";
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
        selectSquare,
        selectDropPiece,
        confirmPromotion,
        cancelPromotion,
        resetGame,
        resign,
        undo,
        redo,
        goToMove,
        canUndo,
        canRedo,
    } = useGameStore();

    return (
        <div className="min-h-screen bg-gray-100 py-4 sm:py-8">
            <div className="container mx-auto px-2 sm:px-4">
                <h1 className="text-2xl sm:text-3xl lg:text-4xl font-bold text-center mb-4 sm:mb-8">
                    将棋
                </h1>

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
                                    resignedPlayer={resignedPlayer}
                                    onReset={resetGame}
                                    onResign={resign}
                                />
                                <MoveHistory
                                    moveHistory={moveHistory}
                                    historyCursor={historyCursor}
                                    canUndo={canUndo()}
                                    canRedo={canRedo()}
                                    onUndo={undo}
                                    onRedo={redo}
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
                <div className="hidden lg:flex gap-6 xl:gap-8 justify-center items-start">
                    {/* 後手の持ち駒 */}
                    <div className="flex-shrink-0">
                        <CapturedPieces
                            hands={hands}
                            player="white"
                            currentPlayer={currentPlayer}
                            selectedDropPiece={selectedDropPiece}
                            onPieceClick={selectDropPiece}
                        />
                    </div>

                    {/* 将棋盤 */}
                    <div className="flex-shrink-0 pr-6 xl:pr-8">
                        <Board
                            board={board}
                            selectedSquare={selectedSquare}
                            validMoves={validMoves}
                            validDropSquares={validDropSquares}
                            onSquareClick={selectSquare}
                        />
                    </div>

                    {/* 右側パネル（先手の持ち駒 + ゲーム情報 + 棋譜） */}
                    <div className="flex-shrink-0 space-y-6">
                        <CapturedPieces
                            hands={hands}
                            player="black"
                            currentPlayer={currentPlayer}
                            selectedDropPiece={selectedDropPiece}
                            onPieceClick={selectDropPiece}
                        />
                        <GameInfo
                            currentPlayer={currentPlayer}
                            gameStatus={gameStatus}
                            moveHistory={moveHistory}
                            resignedPlayer={resignedPlayer}
                            onReset={resetGame}
                            onResign={resign}
                        />
                        <MoveHistory
                            moveHistory={moveHistory}
                            historyCursor={historyCursor}
                            canUndo={canUndo()}
                            canRedo={canRedo()}
                            onUndo={undo}
                            onRedo={redo}
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
            </div>
        </div>
    );
}

export default App;
