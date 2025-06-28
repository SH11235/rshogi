use wasm_bindgen::prelude::*;
use web_sys::{RtcPeerConnection, RtcPeerConnectionIceEvent, RtcConfiguration};
use js_sys::{Array, Object, Reflect};
use futures::channel::mpsc;
use serde::{Serialize, Deserialize};
use base64::{Engine as _, engine::general_purpose};
use std::cell::RefCell;
use std::rc::Rc;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console)]
    fn log(s: &str);
}

macro_rules! console_log {
    ($($t:tt)*) => (log(&format_args!($($t)*).to_string()))
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SignalData {
    pub signal_type: String,
    pub data: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct OfferData {
    pub sdp: String,
    pub peer_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct IceCandidateData {
    pub candidate: String,
    pub sdp_mid: Option<String>,
    pub sdp_m_line_index: Option<u16>,
}

#[wasm_bindgen]
pub struct SimpleWebRTCPeer {
    peer_id: String,
    connection: RtcPeerConnection,
    data_channel: Rc<RefCell<Option<web_sys::RtcDataChannel>>>,
    // For future use: signal channel
    _signal_sender: mpsc::UnboundedSender<SignalData>,
}

#[wasm_bindgen]
impl SimpleWebRTCPeer {
    pub fn get_peer_id(&self) -> String {
        self.peer_id.clone()
    }
    
    pub fn send_message(&self, message: &str) -> Result<(), JsValue> {
        let channel_ref = self.data_channel.borrow();
        if let Some(channel) = channel_ref.as_ref() {
            if channel.ready_state() == web_sys::RtcDataChannelState::Open {
                channel.send_with_str(message)?;
                console_log!("Sent message: {}", message);
            } else {
                console_log!("Data channel not open, state: {:?}", channel.ready_state());
            }
        } else {
            console_log!("No data channel available");
        }
        Ok(())
    }
    
    pub async fn create_offer(&self) -> Result<String, JsValue> {
        let offer = wasm_bindgen_futures::JsFuture::from(
            self.connection.create_offer()
        ).await?;
        
        wasm_bindgen_futures::JsFuture::from(
            self.connection.set_local_description(&offer.into())
        ).await?;
        
        // Wait for ICE gathering to complete
        wait_for_ice_gathering(&self.connection).await?;
        
        let local_desc = self.connection.local_description()
            .ok_or_else(|| JsValue::from_str("No local description"))?;
        
        let offer_data = OfferData {
            sdp: local_desc.sdp(),
            peer_id: self.peer_id.clone(),
        };
        
        let json = serde_json::to_string(&offer_data)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        
        Ok(general_purpose::STANDARD.encode(json))
    }
    
    pub async fn handle_offer(&self, offer_str: &str) -> Result<String, JsValue> {
        let decoded = general_purpose::STANDARD.decode(offer_str)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        
        let json = String::from_utf8(decoded)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        
        let offer_data: OfferData = serde_json::from_str(&json)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        
        let mut offer_desc = web_sys::RtcSessionDescriptionInit::new(web_sys::RtcSdpType::Offer);
        offer_desc.sdp(&offer_data.sdp);
        
        wasm_bindgen_futures::JsFuture::from(
            self.connection.set_remote_description(&offer_desc)
        ).await?;
        
        let answer = wasm_bindgen_futures::JsFuture::from(
            self.connection.create_answer()
        ).await?;
        
        wasm_bindgen_futures::JsFuture::from(
            self.connection.set_local_description(&answer.into())
        ).await?;
        
        // Wait for ICE gathering to complete
        wait_for_ice_gathering(&self.connection).await?;
        
        let local_desc = self.connection.local_description()
            .ok_or_else(|| JsValue::from_str("No local description"))?;
        
        let answer_data = OfferData {
            sdp: local_desc.sdp(),
            peer_id: self.peer_id.clone(),
        };
        
        let json = serde_json::to_string(&answer_data)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        
        Ok(general_purpose::STANDARD.encode(json))
    }
    
    pub async fn handle_answer(&self, answer_str: &str) -> Result<(), JsValue> {
        let decoded = general_purpose::STANDARD.decode(answer_str)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        
        let json = String::from_utf8(decoded)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        
        let answer_data: OfferData = serde_json::from_str(&json)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        
        let mut answer_desc = web_sys::RtcSessionDescriptionInit::new(web_sys::RtcSdpType::Answer);
        answer_desc.sdp(&answer_data.sdp);
        
        wasm_bindgen_futures::JsFuture::from(
            self.connection.set_remote_description(&answer_desc)
        ).await?;
        
        Ok(())
    }
}

#[wasm_bindgen]
pub async fn create_webrtc_peer(is_host: bool) -> Result<SimpleWebRTCPeer, JsValue> {
    console_log!("Creating WebRTC peer, is_host: {}", is_host);
    
    // Generate peer ID
    let peer_id = format!("peer-{}", js_sys::Math::random().to_string().chars().skip(2).take(8).collect::<String>());
    
    // Configure STUN servers
    let ice_servers = Array::new();
    
    let stun_server = Object::new();
    let urls = Array::new();
    urls.push(&JsValue::from_str("stun:stun.l.google.com:19302"));
    urls.push(&JsValue::from_str("stun:stun1.l.google.com:19302"));
    Reflect::set(&stun_server, &JsValue::from_str("urls"), &urls)?;
    ice_servers.push(&stun_server);
    
    let mut config = RtcConfiguration::new();
    config.ice_servers(&ice_servers);
    
    let connection = RtcPeerConnection::new_with_configuration(&config)?;
    let (tx, _rx) = mpsc::unbounded();
    
    // Set up data channel
    let data_channel = Rc::new(RefCell::new(if is_host {
        let channel = connection.create_data_channel("game");
        Some(channel)
    } else {
        None
    }));
    
    let peer = SimpleWebRTCPeer {
        peer_id: peer_id.clone(),
        connection: connection.clone(),
        data_channel: data_channel.clone(),
        _signal_sender: tx,
    };
    
    // Set up connection event handlers
    {
        let data_channel_clone = data_channel.clone();
        let ondatachannel = Closure::wrap(Box::new(move |event: web_sys::RtcDataChannelEvent| {
            console_log!("Data channel received");
            let channel = event.channel();
            
            // Store the received channel for the guest
            *data_channel_clone.borrow_mut() = Some(channel.clone());
            console_log!("Data channel stored for guest");
            
            // Set up channel event handlers
            let onopen = Closure::wrap(Box::new(move || {
                console_log!("Data channel opened");
                dispatch_webrtc_event("channel-open", "");
            }) as Box<dyn FnMut()>);
            channel.set_onopen(Some(onopen.as_ref().unchecked_ref()));
            onopen.forget();
            
            let onmessage = Closure::wrap(Box::new(move |event: web_sys::MessageEvent| {
                if let Ok(text) = event.data().dyn_into::<js_sys::JsString>() {
                    let message: String = text.into();
                    console_log!("Received message: {}", message);
                    dispatch_webrtc_event("message", &message);
                }
            }) as Box<dyn FnMut(_)>);
            channel.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));
            onmessage.forget();
            
        }) as Box<dyn FnMut(_)>);
        connection.set_ondatachannel(Some(ondatachannel.as_ref().unchecked_ref()));
        ondatachannel.forget();
    }
    
    // Set up ICE candidate handler
    {
        let onicecandidate = Closure::wrap(Box::new(move |event: RtcPeerConnectionIceEvent| {
            if let Some(candidate) = event.candidate() {
                console_log!("ICE candidate: {:?}", candidate.candidate());
            }
        }) as Box<dyn FnMut(_)>);
        connection.set_onicecandidate(Some(onicecandidate.as_ref().unchecked_ref()));
        onicecandidate.forget();
    }
    
    // Set up connection state change handler
    {
        let conn_clone = connection.clone();
        let onconnectionstatechange = Closure::wrap(Box::new(move || {
            console_log!("Connection state: {:?}", conn_clone.ice_connection_state());
            dispatch_webrtc_event("connection-state", &format!("{:?}", conn_clone.ice_connection_state()));
        }) as Box<dyn FnMut()>);
        connection.set_onconnectionstatechange(Some(onconnectionstatechange.as_ref().unchecked_ref()));
        onconnectionstatechange.forget();
    }
    
    // Set up data channel handlers if host
    if let Some(channel) = data_channel.borrow().as_ref() {
        let onopen = Closure::wrap(Box::new(move || {
            console_log!("Data channel opened (host)");
            dispatch_webrtc_event("channel-open", "");
        }) as Box<dyn FnMut()>);
        channel.set_onopen(Some(onopen.as_ref().unchecked_ref()));
        onopen.forget();
        
        let onmessage = Closure::wrap(Box::new(move |event: web_sys::MessageEvent| {
            if let Ok(text) = event.data().dyn_into::<js_sys::JsString>() {
                let message: String = text.into();
                console_log!("Received message: {}", message);
                dispatch_webrtc_event("message", &message);
            }
        }) as Box<dyn FnMut(_)>);
        channel.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));
        onmessage.forget();
    }
    
    Ok(peer)
}

async fn wait_for_ice_gathering(_connection: &RtcPeerConnection) -> Result<(), JsValue> {
    // Simple wait for ICE gathering
    // In production, you'd want to properly wait for the gathering to complete
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        let window = web_sys::window().unwrap();
        let closure = Closure::once(Box::new(move || {
            resolve.call0(&JsValue::NULL).unwrap();
        }) as Box<dyn FnOnce()>);
        
        window.set_timeout_with_callback_and_timeout_and_arguments_0(
            closure.as_ref().unchecked_ref(),
            1000  // Wait 1 second for ICE gathering
        ).unwrap();
        
        closure.forget();
    });
    
    wasm_bindgen_futures::JsFuture::from(promise).await?;
    Ok(())
}

fn dispatch_webrtc_event(event_type: &str, data: &str) {
    let window = web_sys::window().expect("no global `window` exists");
    let event_init = web_sys::CustomEventInit::new();
    event_init.set_detail(&JsValue::from_str(data));
    let event = web_sys::CustomEvent::new_with_event_init_dict(
        &format!("webrtc-{}", event_type),
        &event_init
    ).expect("Failed to create custom event");
    window.dispatch_event(&event).expect("Failed to dispatch event");
}