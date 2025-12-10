import { createWasmEngineClient } from "@shogi/engine-wasm";
import type { EngineOption } from "@shogi/ui";
import { EngineControlPanel, PlaygroundPage, ShogiMatch } from "@shogi/ui";

const createEngineClient = () =>
    createWasmEngineClient({
        stopMode: "terminate",
    });

const engineOptions: EngineOption[] = [
    { id: "wasm", label: "内蔵エンジン", createClient: createEngineClient, kind: "internal" },
];

const panelEngine = createEngineClient();

function App() {
    return (
        <PlaygroundPage
            eyebrow="Web / Wasm"
            summary="盤 UI とエンジン操作をひとまとめにしたデバッグページ。Wasm エンジンを startpos から動かしつつ、手入力や棋譜入出力を試せます。"
            note="同じ UI を Desktop でも再利用できるよう構成しています。"
        >
            <ShogiMatch engineOptions={engineOptions} />
            <EngineControlPanel engine={panelEngine} />
        </PlaygroundPage>
    );
}

export default App;
