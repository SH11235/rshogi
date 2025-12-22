import { createWasmEngineClient } from "@shogi/engine-wasm";
import type { EngineOption } from "@shogi/ui";
import { EngineControlPanel, ShogiMatch } from "@shogi/ui";

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
        <main className="mx-auto flex max-w-[1100px] flex-col gap-[14px] px-5 pb-[72px] pt-6">
            <ShogiMatch engineOptions={engineOptions} />
            <EngineControlPanel engine={panelEngine} />
        </main>
    );
}

export default App;
