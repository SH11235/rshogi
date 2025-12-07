import { createTauriEngineClient, getLegalMoves } from "@shogi/engine-tauri";
import { EngineControlPanel, ShogiMatch } from "@shogi/ui";
import "./App.css";

const engineA = createTauriEngineClient({
    stopMode: "terminate",
    useMockOnError: false,
    debug: true,
});
const engineB = createTauriEngineClient({
    stopMode: "terminate",
    useMockOnError: false,
    debug: true,
});

const engineOptions = [
    { id: "native-a", label: "内蔵エンジン A", client: engineA },
    { id: "native-b", label: "内蔵エンジン B", client: engineB },
];

function App() {
    return (
        <main className="page">
            <section className="hero">
                <p className="eyebrow">Desktop / Tauri</p>
                <h1>Shogi Playground</h1>
                <p className="sub">
                    ネイティブエンジンへ IPC で init/loadPosition/search/setoption
                    を送るモーダルと盤 UI の統合デモ。 Web 版と共通の UI を利用しています。
                </p>
                <p className="hint">
                    StrictMode でも自動探索を避けるため、操作はすべて明示的ボタンで発火。
                </p>
            </section>

            <div className="stack">
                <ShogiMatch
                    engineOptions={engineOptions}
                    fetchLegalMoves={(moves) => getLegalMoves({ sfen: "startpos", moves })}
                />
                <EngineControlPanel engine={engineA} />
            </div>
        </main>
    );
}

export default App;
