import { useState } from "react";
import { useP2P } from "../hooks/useP2P";

export function P2PTestPanel() {
    const { isInitialized, isConnected, receivedMessages, connect, sendMessage } = useP2P();
    const [roomId, setRoomId] = useState("shogi-room-1");
    const [message, setMessage] = useState("");

    const handleConnect = () => {
        connect(roomId);
    };

    const handleSendMessage = () => {
        sendMessage(message);
        setMessage("");
    };

    if (!isInitialized) {
        return <div>Initializing Wasm...</div>;
    }

    return (
        <div style={{ border: "1px solid black", padding: "10px", margin: "10px" }}>
            <h3>P2P Test Panel</h3>
            <div>
                <input
                    type="text"
                    value={roomId}
                    onChange={(e) => setRoomId(e.target.value)}
                    placeholder="Room ID"
                    disabled={isConnected}
                />
                <button type="button" onClick={handleConnect} disabled={isConnected}>
                    {isConnected ? "Connected" : "Connect"}
                </button>
            </div>
            <hr style={{ margin: "10px 0" }} />
            <div>
                <input
                    type="text"
                    value={message}
                    onChange={(e) => setMessage(e.target.value)}
                    placeholder="Message to send"
                    disabled={!isConnected}
                />
                <button type="button" onClick={handleSendMessage} disabled={!isConnected}>
                    Send
                </button>
            </div>
            <hr style={{ margin: "10px 0" }} />
            <div>
                <h4>Received Messages:</h4>
                <ul style={{ listStyleType: "none", padding: 0 }}>
                    {receivedMessages.map((msg) => (
                        <li
                            key={msg + Math.random()}
                            style={{ borderBottom: "1px solid #ccc", padding: "5px" }}
                        >
                            {msg}
                        </li>
                    ))}
                </ul>
            </div>
        </div>
    );
}
