import { createWasmEngineClient } from "@shogi/engine-wasm";
import { EngineControlPanel } from "@shogi/ui";
import "./App.css";

const engine = createWasmEngineClient({
    stopMode: "terminate",
});

function App() {
    return (
        <main className="page">
            <section className="hero">
                <p className="eyebrow">Web / Wasm</p>
                <h1>Engine Control Panel</h1>
                <p className="sub">
                    Wasm worker 越しの EngineClient
                    を操作するデバッグ用モーダル。秒読みデフォルト（ponder OFF）で、 setoption /
                    search をまとめて試せます。
                </p>
                <p className="hint">同じ UI を Desktop でも再利用できるよう構成しています。</p>
            </section>

            <EngineControlPanel engine={engine} />
        </main>
    );
}

export default App;
