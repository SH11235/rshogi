use std::sync::Mutex;
use tauri::async_runtime::JoinHandle;
use tauri::State;
use tauri::{Manager, Window};

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

#[derive(serde::Serialize)]
#[serde(tag = "type")]
enum EngineEvent {
    #[serde(rename = "info")]
    Info {
        depth: Option<u32>,
        nodes: Option<u64>,
        nps: Option<u64>,
        scoreCp: Option<i32>,
        scoreMate: Option<i32>,
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
    let mut inner = state.inner.lock().map_err(|e| e.to_string())?;
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
    let mut inner = state.inner.lock().map_err(|e| e.to_string())?;
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
        let mut inner = state.inner.lock().map_err(|e| e.to_string())?;
        if let Some(handle) = inner.handle.take() {
            handle.abort();
        }
        let pos = inner.position.clone();
        let _ = params; // TODO: use params for time controls when native engine is wired.
        // Spawn a tiny mock search: emit one info and one bestmove.
        inner.handle = Some(tauri::async_runtime::spawn(async move {
            window
                .emit_all(
                    ENGINE_EVENT,
                    EngineEvent::Info {
                        depth: Some(1),
                        nodes: Some(128),
                        nps: Some(2048),
                        scoreCp: Some(0),
                        scoreMate: None,
                    },
                )
                .ok();

            tauri::async_runtime::sleep(std::time::Duration::from_millis(50)).await;

            let mv = if pos.is_empty() { "resign" } else { "resign" };
            window
                .emit_all(
                    ENGINE_EVENT,
                    EngineEvent::BestMove {
                        mv: mv.to_string(),
                        ponder: None,
                    },
                )
                .ok();
        }));
    }

    Ok(())
}

#[tauri::command]
fn engine_stop(state: State<EngineState>) -> Result<(), String> {
    let mut inner = state.inner.lock().map_err(|e| e.to_string())?;
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
