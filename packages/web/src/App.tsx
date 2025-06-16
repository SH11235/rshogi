import "./App.css";
import { Board } from "./components/Board";
import { CapturedPieces } from "./components/CapturedPieces";
import { GameInfo } from "./components/GameInfo";
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
        promotionPending,
        selectSquare,
        selectDropPiece,
        confirmPromotion,
        cancelPromotion,
        resetGame,
    } = useGameStore();

    return (
        <div className="min-h-screen bg-gray-100 py-8">
            <div className="container mx-auto px-4">
                <h1 className="text-4xl font-bold text-center mb-8">将棋</h1>

                <div className="flex flex-col lg:flex-row gap-8 justify-center items-start">
                    {/* 後手の持ち駒 */}
                    <div className="order-2 lg:order-1">
                        <CapturedPieces
                            hands={hands}
                            player="white"
                            currentPlayer={currentPlayer}
                            selectedDropPiece={selectedDropPiece}
                            onPieceClick={selectDropPiece}
                        />
                    </div>

                    {/* 将棋盤 */}
                    <div className="order-1 lg:order-2">
                        <Board
                            board={board}
                            selectedSquare={selectedSquare}
                            validMoves={validMoves}
                            validDropSquares={validDropSquares}
                            onSquareClick={selectSquare}
                        />
                    </div>

                    {/* 先手の持ち駒 */}
                    <div className="order-3">
                        <CapturedPieces
                            hands={hands}
                            player="black"
                            currentPlayer={currentPlayer}
                            selectedDropPiece={selectedDropPiece}
                            onPieceClick={selectDropPiece}
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
