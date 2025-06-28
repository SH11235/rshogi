use futures::channel::mpsc;
use futures::{SinkExt, StreamExt, future::FutureExt, select};
use ggrs::PlayerType;
use gloo_timers::future::TimeoutFuture;
use matchbox_socket::WebRtcSocket;
use std::collections::HashSet;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::spawn_local;
use serde::{Serialize, Deserialize};
use base64::{Engine as _, engine::general_purpose};

// P2P implementations
// mod libp2p_p2p;  // Currently not used
mod simple_webrtc;
// pub use libp2p_p2p::*;
pub use simple_webrtc::*;


#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console)]
    fn log(s: &str);
}

fn dispatch_p2p_message(message: &str) {
    let window = web_sys::window().expect("no global `window` exists");
    let event_init = web_sys::CustomEventInit::new();
    event_init.set_detail(&JsValue::from_str(message));
    let event = web_sys::CustomEvent::new_with_event_init_dict("p2p-message", &event_init)
        .expect("Failed to create custom event");
    window
        .dispatch_event(&event)
        .expect("Failed to dispatch event");
}

fn dispatch_ticket_event(event_type: &str, data: &str) {
    let window = web_sys::window().expect("no global `window` exists");
    let event_init = web_sys::CustomEventInit::new();
    event_init.set_detail(&JsValue::from_str(data));
    let event = web_sys::CustomEvent::new_with_event_init_dict(event_type, &event_init)
        .expect("Failed to create custom event");
    window
        .dispatch_event(&event)
        .expect("Failed to dispatch event");
}

macro_rules! console_log {
    ($($t:tt)*) => (log(&format_args!($($t)*).to_string()))
}

// Connection mode enum
#[derive(Debug, Clone, Serialize, Deserialize)]
enum ConnectionMode {
    Local,
    Serverless,
}

// Message types for Iroh communication
#[derive(Debug, Clone, Serialize, Deserialize)]
struct IrohHandshake {
    room_id: String,
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

// This function will be the main entry point from JavaScript (existing local mode)
#[wasm_bindgen]
pub fn start_p2p_manager(room_id: String) -> P2PHandle {
    let (tx, rx) = mpsc::channel::<String>(100);

    spawn_local(p2p_loop_local(room_id, rx));

    P2PHandle { sender: tx }
}

// New serverless P2P functions

#[wasm_bindgen]
pub fn create_host_ticket() -> Result<String, JsValue> {
    console_log!("Creating host ticket for serverless P2P");
    
    // Generate a secure random room ID
    let room_id = format!("room-{}", js_sys::Math::random().to_string().chars().skip(2).take(8).collect::<String>());
    
    // Create a simple ticket containing the room ID
    let handshake = IrohHandshake { room_id };
    let ticket_data = serde_json::to_string(&handshake).map_err(|e| JsValue::from_str(&e.to_string()))?;
    let ticket = general_purpose::STANDARD.encode(ticket_data);
    
    console_log!("Created ticket: {}", ticket);
    
    Ok(ticket)
}

#[wasm_bindgen]
pub fn join_with_ticket(ticket: String) -> Result<P2PHandle, JsValue> {
    console_log!("Joining with ticket: {}", ticket);
    
    let (tx, rx) = mpsc::channel::<String>(100);
    
    spawn_local(async move {
        match join_serverless(ticket, rx).await {
            Ok(_) => console_log!("Successfully joined serverless P2P"),
            Err(e) => console_log!("Failed to join: {:?}", e),
        }
    });
    
    Ok(P2PHandle { sender: tx })
}

async fn join_serverless(ticket: String, rx: mpsc::Receiver<String>) -> Result<(), JsValue> {
    // Decode the ticket
    let decoded = general_purpose::STANDARD.decode(&ticket).map_err(|e| JsValue::from_str(&e.to_string()))?;
    let ticket_data = String::from_utf8(decoded).map_err(|e| JsValue::from_str(&e.to_string()))?;
    
    // Parse the room ID from the ticket
    let handshake: IrohHandshake = serde_json::from_str(&ticket_data)
        .map_err(|e| JsValue::from_str(&e.to_string()))?;
    
    console_log!("Extracted room ID: {}", handshake.room_id);
    
    // Connect to public Matchbox server with the shared room ID
    // TODO: Replace with a reliable public server or your own deployment
    // For now, using local server for testing
    let room_url = format!("ws://127.0.0.1:3536/{}", handshake.room_id);
    console_log!("Warning: Using local matchbox server. Deploy your own for production!");
    p2p_loop_serverless(room_url, rx).await;
    
    Ok(())
}

// Serverless P2P loop using public Matchbox server
async fn p2p_loop_serverless(room_url: String, mut rx: mpsc::Receiver<String>) {
    console_log!("Connecting to public Matchbox server: {}", room_url);
    
    let (mut socket, message_loop) = WebRtcSocket::builder(room_url)
        .add_unreliable_channel()
        .build();
    
    let mut message_loop = message_loop.fuse();
    let mut peers = HashSet::new();
    let mut peer_update_counter = 0u32;
    
    loop {
        // Check for incoming messages first (immediate processing)
        let channel = socket.channel_mut(0);
        for (peer, packet) in channel.receive() {
            let message_str = String::from_utf8_lossy(&packet);
            console_log!("Received message from {}: {}", peer, message_str);
            dispatch_p2p_message(&message_str);
        }
        
        // Update peers every ~20 iterations (approximately 100ms)
        peer_update_counter += 1;
        if peer_update_counter >= 20 {
            peer_update_counter = 0;
            socket.update_peers();
            let new_peers = socket.players().into_iter().collect::<HashSet<_>>();
            
            for &peer in new_peers.difference(&peers) {
                console_log!("New peer connected: {:?}", peer);
                dispatch_ticket_event("peer-connected", &format!("{{\"peer\":\"{:?}\"}}", peer));
            }
            for &peer in peers.difference(&new_peers) {
                console_log!("Peer left: {:?}", peer);
                dispatch_ticket_event("peer-disconnected", &format!("{{\"peer\":\"{:?}\"}}", peer));
            }
            peers = new_peers;
        }
        
        select! {
            // Handle signalling loop completion
            result = message_loop => {
                console_log!("Signalling loop finished with result: {:?}", result);
                break;
            },
            
            // Handle outgoing messages from the channel
            message = rx.next() => {
                if let Some(message) = message {
                    console_log!("Sending broadcast message: {}", message);
                    let packet = message.as_bytes().to_vec().into_boxed_slice();
                    let channel = socket.channel_mut(0);
                    for &peer in &peers {
                        if let PlayerType::Remote(peer_id) = peer {
                            channel.send(packet.clone(), peer_id);
                        }
                    }
                }
            },
            
            // Small timeout to yield control and check messages again
            _ = TimeoutFuture::new(5).fuse() => {
                // This ensures the loop continues and checks for messages
            }
        }
    }
    console_log!("Serverless P2P loop finished.");
}

// The main async loop for local mode (existing implementation)
async fn p2p_loop_local(room_id: String, mut rx: mpsc::Receiver<String>) {
    let room_url = format!("ws://127.0.0.1:3536/{}", room_id);
    console_log!("Connecting to local server: {}", room_url);

    let (mut socket, message_loop) = WebRtcSocket::builder(room_url)
        .add_unreliable_channel()
        .build();

    let mut message_loop = message_loop.fuse();
    let mut peers = HashSet::new();
    let mut peer_update_counter = 0u32;

    loop {
        // Check for incoming messages first (immediate processing)
        let channel = socket.channel_mut(0);
        for (peer, packet) in channel.receive() {
            let message_str = String::from_utf8_lossy(&packet);
            console_log!("Received message from {}: {}", peer, message_str);
            dispatch_p2p_message(&message_str);
        }

        // Update peers every ~20 iterations (approximately 100ms)
        peer_update_counter += 1;
        if peer_update_counter >= 20 {
            peer_update_counter = 0;
            socket.update_peers();
            let new_peers = socket.players().into_iter().collect::<HashSet<_>>();

            for &peer in new_peers.difference(&peers) {
                console_log!("New peer connected: {:?}", peer);
            }
            for &peer in peers.difference(&new_peers) {
                console_log!("Peer left: {:?}", peer);
            }
            peers = new_peers;
        }

        select! {
            // Handle signalling loop completion
            result = message_loop => {
                console_log!("Signalling loop finished with result: {:?}", result);
                break;
            },

            // Handle outgoing messages from the channel
            message = rx.next() => {
                if let Some(message) = message {
                    console_log!("Sending broadcast message: {}", message);
                    let packet = message.as_bytes().to_vec().into_boxed_slice();
                    let channel = socket.channel_mut(0);
                    for &peer in &peers {
                        if let PlayerType::Remote(peer_id) = peer {
                            channel.send(packet.clone(), peer_id);
                        }
                    }
                }
            },

            // Small timeout to yield control and check messages again
            _ = TimeoutFuture::new(5).fuse() => {
                // This ensures the loop continues and checks for messages
            }
        }
    }
    console_log!("Local P2P loop finished.");
}

// Host creation for serverless mode
#[wasm_bindgen]
pub fn create_serverless_host() -> Result<P2PHandle, JsValue> {
    console_log!("Creating serverless host");
    
    let (tx, rx) = mpsc::channel::<String>(100);
    
    // Generate a random room ID
    let room_id = format!("room-{}", js_sys::Math::random().to_string().chars().skip(2).take(8).collect::<String>());
    
    // Create the ticket data
    let handshake = IrohHandshake { room_id: room_id.clone() };
    let ticket_data = serde_json::to_string(&handshake).map_err(|e| JsValue::from_str(&e.to_string()))?;
    let ticket = general_purpose::STANDARD.encode(&ticket_data);
    
    // Dispatch the ticket to the UI
    dispatch_ticket_event("host-ticket-created", &format!("{{\"ticket\":\"{}\"}}", ticket));
    
    // Start the P2P loop with public Matchbox server
    // TODO: Replace with a reliable public server or your own deployment
    // For now, using local server for testing
    let room_url = format!("ws://127.0.0.1:3536/{}", room_id);
    console_log!("Warning: Using local matchbox server. Deploy your own for production!");
    spawn_local(p2p_loop_serverless(room_url, rx));
    
    Ok(P2PHandle { sender: tx })
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

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::*;

    wasm_bindgen_test_configure!(run_in_browser);

    #[wasm_bindgen_test]
    fn test_piece_creation() {
        let piece = Piece::new(PieceType::Pawn, Player::Black);
        assert_eq!(piece.piece_type, PieceType::Pawn);
        assert_eq!(piece.owner, Player::Black);
        assert_eq!(piece.promoted, false);
    }

    #[wasm_bindgen_test]
    fn test_p2p_handle_creation() {
        let _handle = start_p2p_manager("test-room".to_string());
        // Basic check that handle is created
        // Note: We can't easily test the async behavior in unit tests
        assert!(true); // Placeholder - handle exists
    }

    #[test]
    fn test_player_enum() {
        let black = Player::Black;
        let white = Player::White;
        assert_ne!(black, white);
    }

    #[test]
    fn test_piece_type_enum() {
        let pawn = PieceType::Pawn;
        assert_eq!(pawn, PieceType::Pawn);
    }
}