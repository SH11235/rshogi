import { useCallback, useEffect, useState, useSyncExternalStore } from "react";
import initWasm, { type P2PHandle, start_p2p_manager } from "shogi-rust-core";
import { p2pStore } from "../stores/p2pStore";

export function useP2P() {
    const [handle, setHandle] = useState<P2PHandle | null>(null);
    const receivedMessages = useSyncExternalStore(p2pStore.subscribe, p2pStore.getSnapshot);
    const [isConnected, setIsConnected] = useState(false);
    const [isInitialized, setIsInitialized] = useState(false);

    useEffect(() => {
        initWasm().then(() => setIsInitialized(true));
    }, []);

    const connect = useCallback(
        (roomId: string) => {
            if (isConnected || !isInitialized) return;

            console.log("Starting P2P manager for room:", roomId);
            const p2pHandle = start_p2p_manager(roomId);
            setHandle(p2pHandle);
            setIsConnected(true);
        },
        [isConnected, isInitialized],
    );

    const sendMessage = useCallback(
        (message: string) => {
            if (handle && isConnected) {
                handle.send_message(message);
            } else {
                console.error("Cannot send message, not connected or handle not ready.");
            }
        },
        [handle, isConnected],
    );

    return {
        isInitialized,
        isConnected,
        receivedMessages,
        connect,
        sendMessage,
    };
}
