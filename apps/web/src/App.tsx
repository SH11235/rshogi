import { createWasmEngineClient } from "@shogi/engine-wasm";
import { EngineControlPanel, ShogiMatch } from "@shogi/ui";
import "./App.css";

const engineA = createWasmEngineClient({
    stopMode: "terminate",
});
const engineB = createWasmEngineClient({
    stopMode: "terminate",
});

const engineOptions = [
    { id: "wasm-a", label: "内蔵エンジン A", client: engineA },
    { id: "wasm-b", label: "内蔵エンジン B", client: engineB },
];

function App() {
    return (
        <main className="page">
            <section className="hero">
                <p className="eyebrow">Web / Wasm</p>
                <h1>Shogi Playground</h1>
                <p className="sub">
                    盤 UI とエンジン操作をひとまとめにしたデバッグページ。Wasm エンジンを startpos
                    から動かしつつ、 手入力や棋譜入出力を試せます。
                </p>
                <p className="hint">同じ UI を Desktop でも再利用できるよう構成しています。</p>
            </section>

            <div className="stack">
                <ShogiMatch engineOptions={engineOptions} />
                <EngineControlPanel engine={engineA} />
            </div>
        </main>
    );
}

export default App;
