use std::sync::Mutex;
use tauri::async_runtime::JoinHandle;
use tauri::{Emitter, State, Window};

const ENGINE_EVENT: &str = "engine://event";

#[derive(Default)]
struct EngineState {
    inner: Mutex<EngineStateInner>,
}

#[derive(Default)]
struct EngineStateInner {
    position: String,
    handle: Option<JoinHandle<()>>,
}

#[derive(serde::Serialize, Clone)]
#[serde(tag = "type")]
enum EngineEvent {
    #[serde(rename = "info")]
    Info {
        depth: Option<u32>,
        nodes: Option<u64>,
        nps: Option<u64>,
        #[serde(rename = "scoreCp")]
        score_cp: Option<i32>,
        #[serde(rename = "scoreMate")]
        score_mate: Option<i32>,
    },
    #[serde(rename = "bestmove")]
    BestMove {
        #[serde(rename = "move")]
        mv: String,
        ponder: Option<String>,
    },
    #[serde(rename = "error")]
    Error { message: String },
}

#[tauri::command]
fn engine_init(state: State<EngineState>, opts: Option<serde_json::Value>) -> Result<(), String> {
    let mut inner = state
        .inner
        .lock()
        .map_err(|e| format!("Failed to acquire engine state lock: {e}"))?;
    inner.position = "startpos".to_string();
    let _ = opts; // TODO: map options when native engine is wired.
    Ok(())
}

#[tauri::command]
fn engine_position(
    state: State<EngineState>,
    sfen: String,
    moves: Option<Vec<String>>,
) -> Result<(), String> {
    let mut inner = state
        .inner
        .lock()
        .map_err(|e| format!("Failed to acquire engine state lock: {e}"))?;
    inner.position = if let Some(moves) = moves {
        format!("{} moves {}", sfen, moves.join(" "))
    } else {
        sfen
    };
    Ok(())
}

#[tauri::command]
fn engine_option(_name: String, _value: serde_json::Value) -> Result<(), String> {
    // TODO: map to real engine options when native engine is wired.
    Ok(())
}

#[tauri::command]
async fn engine_search(
    window: Window,
    state: State<'_, EngineState>,
    params: serde_json::Value,
) -> Result<(), String> {
    // stop any existing search
    {
        let mut inner = state
            .inner
            .lock()
            .map_err(|e| format!("Failed to acquire engine state lock: {e}"))?;
        if let Some(handle) = inner.handle.take() {
            handle.abort();
        }
        let _pos = inner.position.clone();
        let _ = params; // TODO: use params for time controls when native engine is wired.
        // Spawn a tiny mock search: emit one info and one bestmove.
        inner.handle = Some(tauri::async_runtime::spawn(async move {
            window
                .emit(
                    ENGINE_EVENT,
                    EngineEvent::Info {
                        depth: Some(1),
                        nodes: Some(128),
                        nps: Some(2048),
                        score_cp: Some(0),
                        score_mate: None,
                    },
                )
                .unwrap_or_else(|e| eprintln!("Failed to emit engine info event: {e:?}"));

            // TODO: Replace with actual move generation when engine is wired.
            let mv = "resign";
            window
                .emit(
                    ENGINE_EVENT,
                    EngineEvent::BestMove {
                        mv: mv.to_string(),
                        ponder: None,
                    },
                )
                .unwrap_or_else(|e| eprintln!("Failed to emit engine bestmove event: {e:?}"));
        }));
    }

    Ok(())
}

#[tauri::command]
fn engine_stop(state: State<EngineState>) -> Result<(), String> {
    let mut inner = state
        .inner
        .lock()
        .map_err(|e| format!("Failed to acquire engine state lock: {e}"))?;
    if let Some(handle) = inner.handle.take() {
        handle.abort();
    }
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(EngineState::default())
        .invoke_handler(tauri::generate_handler![
            engine_init,
            engine_position,
            engine_search,
            engine_stop,
            engine_option
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
