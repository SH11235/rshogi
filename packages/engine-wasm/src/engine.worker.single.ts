import initWasm, {
    apply_moves as applyMoves,
    dispose as disposeEngine,
    init as initEngine,
    load_model as loadModel,
    load_position as loadPosition,
    search as runSearch,
    set_event_handler as setEventHandler,
    set_option as setOption,
    stop as stopEngine,
} from "../pkg/engine_wasm.js";
import { createEngineWorker } from "./engine.worker.base";

createEngineWorker({
    initWasm,
    applyMoves,
    disposeEngine,
    initEngine,
    loadModel,
    loadPosition,
    runSearch,
    setEventHandler,
    setOption,
    stopEngine,
    defaultWasmModuleUrl: new URL("../pkg/engine_wasm_bg.wasm", import.meta.url),
});
