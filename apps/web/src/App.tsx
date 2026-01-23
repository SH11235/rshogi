import { createIndexedDBNnueStorage, createWasmEngineClient } from "@shogi/engine-wasm";
import type { EngineOption } from "@shogi/ui";
import { EngineControlPanel, NnueProvider, ShogiMatch, useDevMode } from "@shogi/ui";

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
        defaultInitOptions: { threads: wasmThreads },
        logWarningsToConsole: true,
    });

const engineOptions: EngineOption[] = [
    { id: "wasm", label: "内蔵エンジン", createClient: createEngineClient, kind: "internal" },
];

const panelEngine = createEngineClient();

// Web版のストレージも同期的に初期化可能
const nnueStorage = createIndexedDBNnueStorage();

// NNUE プリセット manifest.json の URL（環境変数で設定）
const nnueManifestUrl = import.meta.env.VITE_NNUE_MANIFEST_URL as string | undefined;

function App() {
    const isDevMode = useDevMode();

    return (
        <NnueProvider storage={nnueStorage}>
            <main className="mx-auto flex max-w-[1100px] flex-col gap-3 md:px-5">
                <ShogiMatch
                    engineOptions={engineOptions}
                    isDevMode={isDevMode}
                    manifestUrl={nnueManifestUrl}
                />
                {isDevMode && <EngineControlPanel engine={panelEngine} />}
            </main>
        </NnueProvider>
    );
}

export default App;
