import { createTauriEngineClient, getLegalMoves } from "@shogi/engine-tauri";
import { EngineControlPanel, PlaygroundPage, ShogiMatch } from "@shogi/ui";

const createEngineClient = () =>
    createTauriEngineClient({
        stopMode: "terminate",
        useMockOnError: false,
        debug: true,
    });

const engineOptions = [
    { id: "native", label: "内蔵エンジン", createClient: createEngineClient, kind: "internal" },
];

const panelEngine = createEngineClient();

function App() {
    return (
        <PlaygroundPage
            eyebrow="Desktop / Tauri"
            summary="Tauri で内蔵エンジンと盤 UI の動作を確認する画面です。"
        >
            <ShogiMatch
                engineOptions={engineOptions}
                fetchLegalMoves={(moves) => getLegalMoves({ sfen: "startpos", moves })}
            />
            <EngineControlPanel engine={panelEngine} />
        </PlaygroundPage>
    );
}

export default App;
