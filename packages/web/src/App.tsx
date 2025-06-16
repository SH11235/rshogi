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
        selectSquare,
        selectDropPiece,
        confirmPromotion,
        cancelPromotion,
        resetGame,
        undo,
        redo,
        goToMove,
        canUndo,
        canRedo,
    } = useGameStore();

    return (
        <div className="min-h-screen bg-gray-100 py-8">
            <div className="container mx-auto px-4">
                <h1 className="text-4xl font-bold text-center mb-8">将棋</h1>

                <div className="flex flex-col xl:flex-row gap-8 justify-center items-start">
                    {/* 後手の持ち駒 */}
                    <div className="order-2 xl:order-1">
                        <CapturedPieces
                            hands={hands}
                            player="white"
                            currentPlayer={currentPlayer}
                            selectedDropPiece={selectedDropPiece}
                            onPieceClick={selectDropPiece}
                        />
                    </div>

                    {/* 将棋盤 */}
                    <div className="order-1 xl:order-2">
                        <Board
                            board={board}
                            selectedSquare={selectedSquare}
                            validMoves={validMoves}
                            validDropSquares={validDropSquares}
                            onSquareClick={selectSquare}
                        />
                    </div>

                    {/* 先手の持ち駒 */}
                    <div className="order-3 xl:order-3">
                        <CapturedPieces
                            hands={hands}
                            player="black"
                            currentPlayer={currentPlayer}
                            selectedDropPiece={selectedDropPiece}
                            onPieceClick={selectDropPiece}
                        />
                    </div>

                    {/* 棋譜・移動履歴 */}
                    <div className="order-4 xl:order-4">
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

                {/* ゲーム情報 */}
                <div className="mt-8 flex justify-center">
                    <GameInfo
                        currentPlayer={currentPlayer}
                        gameStatus={gameStatus}
                        moveHistory={moveHistory}
                        onReset={resetGame}
                    />
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
