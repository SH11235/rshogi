import { createWasmEngineClient } from "@shogi/engine-wasm";
import type { EngineOption } from "@shogi/ui";
import { EngineControlPanel, ShogiMatch } from "@shogi/ui";

const resolveWasmThreads = () => {
    const fallback = import.meta.env.DEV ? 4 : 1;
    const raw = import.meta.env.VITE_WASM_THREADS;
    if (typeof raw !== "string" || raw.trim() === "") return fallback;
    const parsed = Number(raw);
    if (!Number.isFinite(parsed) || parsed < 1) return fallback;
    return Math.trunc(parsed);
};

const wasmThreads = resolveWasmThreads();

const createEngineClient = () =>
    createWasmEngineClient({
        stopMode: "terminate",
        threads: wasmThreads,
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
