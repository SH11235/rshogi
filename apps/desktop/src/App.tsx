import {
    createTauriEngineClient,
    createTauriNnueStorage,
    getLegalMoves,
} from "@shogi/engine-tauri";
import type { EngineOption } from "@shogi/ui";
import { EngineControlPanel, NnueProvider, ShogiMatch } from "@shogi/ui";

const createEngineClient = () =>
    createTauriEngineClient({
        stopMode: "terminate",
        useMockOnError: false,
        debug: true,
    });

const engineOptions: EngineOption[] = [
    { id: "native", label: "内蔵エンジン", createClient: createEngineClient, kind: "internal" },
];

const panelEngine = createEngineClient();

// Tauri版のストレージは同期的に初期化可能
const nnueStorage = createTauriNnueStorage();

function App() {
    // デスクトップ版は常に開発者モードを有効化
    return (
        <NnueProvider storage={nnueStorage} platform="desktop">
            <main className="mx-auto flex max-w-[1100px] flex-col gap-3 md:px-5">
                <ShogiMatch
                    engineOptions={engineOptions}
                    fetchLegalMoves={(sfen, moves, options) =>
                        getLegalMoves({ sfen, moves, passRights: options?.passRights })
                    }
                    isDevMode={true}
                />
                <EngineControlPanel engine={panelEngine} />
            </main>
        </NnueProvider>
    );
}

export default App;
