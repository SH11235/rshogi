import "./App.css";
import { Board } from "./components/Board";
import { CapturedPieces } from "./components/CapturedPieces";
import { GameInfo } from "./components/GameInfo";
import { useGameStore } from "./stores/gameStore";

function App() {
    const {
        board,
        hands,
        currentPlayer,
        selectedSquare,
        validMoves,
        gameStatus,
        moveHistory,
        selectSquare,
        resetGame,
    } = useGameStore();

    return (
        <div className="min-h-screen bg-gray-100 py-8">
            <div className="container mx-auto px-4">
                <h1 className="text-4xl font-bold text-center mb-8">将棋ゲーム</h1>

                <div className="flex flex-col lg:flex-row gap-8 justify-center items-start">
                    {/* 後手の持ち駒 */}
                    <div className="order-2 lg:order-1">
                        <CapturedPieces hands={hands} player="white" />
                    </div>

                    {/* 将棋盤 */}
                    <div className="order-1 lg:order-2">
                        <Board
                            board={board}
                            selectedSquare={selectedSquare}
                            validMoves={validMoves}
                            onSquareClick={selectSquare}
                        />
                    </div>

                    {/* 先手の持ち駒 */}
                    <div className="order-3">
                        <CapturedPieces hands={hands} player="black" />
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
            </div>
        </div>
    );
}

export default App;
