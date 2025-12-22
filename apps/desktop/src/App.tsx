import { createTauriEngineClient, getLegalMoves } from "@shogi/engine-tauri";
import type { EngineOption } from "@shogi/ui";
import { EngineControlPanel, ShogiMatch } from "@shogi/ui";

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

function App() {
    return (
        <main className="mx-auto flex max-w-[1100px] flex-col gap-[14px] px-5 pb-[72px] pt-6">
            <ShogiMatch
                engineOptions={engineOptions}
                fetchLegalMoves={(sfen, moves) => getLegalMoves({ sfen, moves })}
            />
            <EngineControlPanel engine={panelEngine} />
        </main>
    );
}

export default App;
