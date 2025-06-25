use futures::channel::mpsc;
use futures::{future::FutureExt, select, SinkExt, StreamExt};
use gloo_timers::future::TimeoutFuture;
use ggrs::PlayerType;
use matchbox_socket::WebRtcSocket;
use std::collections::HashSet;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::spawn_local;
use web_sys;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console)]
    fn log(s: &str);
}

fn dispatch_p2p_message(message: &str) {
    let window = web_sys::window().expect("no global `window` exists");
    let event = web_sys::CustomEvent::new_with_event_init_dict(
        "p2p-message",
        &web_sys::CustomEventInit::new().detail(&JsValue::from_str(message)),
    ).expect("Failed to create custom event");
    window.dispatch_event(&event).expect("Failed to dispatch event");
}

macro_rules! console_log {
    ($($t:tt)*) => (log(&format_args!($($t)*).to_string()))
}

// This struct will hold the sender part of the channel
#[wasm_bindgen]
pub struct P2PHandle {
    sender: mpsc::Sender<String>,
}

#[wasm_bindgen]
impl P2PHandle {
    pub fn send_message(&self, message: String) {
        let mut sender = self.sender.clone();
        spawn_local(async move {
            if let Err(e) = sender.send(message).await {
                console_log!("Failed to send message: {}", e);
            }
        });
    }
}

// This function will be the main entry point from JavaScript
#[wasm_bindgen]
pub fn start_p2p_manager(room_id: String) -> P2PHandle {
    let (tx, rx) = mpsc::channel::<String>(100);

    spawn_local(p2p_loop(room_id, rx));

    P2PHandle { sender: tx }
}

// The main async loop, completely detached from any struct lifetime
async fn p2p_loop(room_id: String, mut rx: mpsc::Receiver<String>) {
    let room_url = format!("ws://127.0.0.1:3536/{}", room_id);
    console_log!("Connecting to: {}", room_url);

    let (mut socket, message_loop) = WebRtcSocket::builder(room_url)
        .add_unreliable_channel()
        .build();

    let mut message_loop = message_loop.fuse();
    let mut peers = HashSet::new();

    loop {
        let mut main_loop = Box::pin(async {
            TimeoutFuture::new(10).await;
            socket.update_peers();
            let new_peers = socket.players().into_iter().collect::<HashSet<_>>();

            for &peer in new_peers.difference(&peers) {
                console_log!("New peer connected: {:?}", peer);
            }
            for &peer in peers.difference(&new_peers) {
                console_log!("Peer left: {:?}", peer);
            }
            peers = new_peers;

            let channel = socket.channel_mut(0);
            for (peer, packet) in channel.receive() {
                let message_str = String::from_utf8_lossy(&packet);
                console_log!("Received message from {}: {}", peer, message_str);
                dispatch_p2p_message(&message_str);
            }

            if let Some(message) = rx.next().await {
                console_log!("Sending broadcast message: {}", message);
                let packet = message.as_bytes().to_vec().into_boxed_slice();
                for &peer in &peers {
                    if let PlayerType::Remote(peer_id) = peer {
                        channel.send(packet.clone(), peer_id);
                    }
                }
            }
        });

        select! {
            result = message_loop => {
                console_log!("Signalling loop finished with result: {:?}", result);
                break;
            },
            _ = main_loop.fuse() => {},
        }
    }
    console_log!("P2P loop finished.");
}

// Dummy structs are unchanged
#[wasm_bindgen]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PieceType {
    Pawn,
}

#[wasm_bindgen]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Player {
    Black,
    White,
}

#[wasm_bindgen]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Piece {
    pub piece_type: PieceType,
    pub owner: Player,
    pub promoted: bool,
}

#[wasm_bindgen]
impl Piece {
    #[wasm_bindgen(constructor)]
    pub fn new(piece_type: PieceType, owner: Player) -> Piece {
        Piece {
            piece_type,
            owner,
            promoted: false,
        }
    }
}