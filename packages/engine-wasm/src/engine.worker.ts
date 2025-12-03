import {
    EngineEvent,
    EngineInitOptions,
    SearchParams,
    createMockEngineClient,
} from "@shogi/engine-client";

type WorkerCommand =
    | { type: "init"; opts?: EngineInitOptions }
    | { type: "loadPosition"; sfen: string; moves?: string[] }
    | { type: "search"; params: SearchParams }
    | { type: "stop" }
    | { type: "dispose" };

const engine = createMockEngineClient();
let handle: Awaited<ReturnType<typeof engine.search>> | null = null;

function postEvent(event: EngineEvent) {
    // eslint-disable-next-line no-restricted-globals
    self.postMessage({ type: "event", payload: event });
}

engine.subscribe(postEvent);

self.onmessage = async (msg: MessageEvent<WorkerCommand>) => {
    const command = msg.data;
    try {
        switch (command.type) {
            case "init":
                await engine.init(command.opts);
                break;
            case "loadPosition":
                await engine.loadPosition(command.sfen, command.moves);
                break;
            case "search":
                if (handle) {
                    await handle.cancel().catch(() => undefined);
                    handle = null;
                }
                handle = await engine.search(command.params);
                break;
            case "stop":
                if (handle) {
                    await handle.cancel().catch(() => undefined);
                    handle = null;
                }
                await engine.stop();
                break;
            case "dispose":
                if (handle) {
                    await handle.cancel().catch(() => undefined);
                    handle = null;
                }
                await engine.dispose();
                break;
            default:
                break;
        }
    } catch (error) {
        postEvent({ type: "error", message: String(error) });
    }
};
