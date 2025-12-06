import { createTauriEngineClient } from "@shogi/engine-tauri";
import { EngineControlPanel } from "@shogi/ui";
import "./App.css";

const engine = createTauriEngineClient({
    stopMode: "terminate",
    useMockOnError: false,
    debug: true,
});

function App() {
    return (
        <main className="page">
            <section className="hero">
                <p className="eyebrow">Desktop / Tauri</p>
                <h1>Engine Control Panel</h1>
                <p className="sub">
                    ネイティブエンジンへ IPC で init/loadPosition/search/setoption
                    を送るモーダル。Wasm 版と共通の UI を利用しています。
                </p>
                <p className="hint">
                    StrictMode でも自動探索を避けるため、操作はすべて明示的ボタンで発火。
                </p>
            </section>

            <EngineControlPanel engine={engine} />
        </main>
    );
}

export default App;
