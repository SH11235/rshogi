import { useCallback, useEffect, useState, useSyncExternalStore } from "react";
import { p2pStore } from "../stores/p2pStore";
import initWasm, {
    type P2PHandle,
    start_p2p_manager,
    create_serverless_host,
    join_with_ticket,
} from "../wasm/shogi_core";

export type ConnectionMode = "local" | "serverless";

export function useP2P() {
    const [handle, setHandle] = useState<P2PHandle | null>(null);
    const receivedMessages = useSyncExternalStore(p2pStore.subscribe, p2pStore.getSnapshot);
    const [isConnected, setIsConnected] = useState(false);
    const [isInitialized, setIsInitialized] = useState(false);
    const [connectionMode, setConnectionMode] = useState<ConnectionMode>("serverless");
    const [hostTicket, setHostTicket] = useState<string | null>(null);

    useEffect(() => {
        initWasm().then(() => setIsInitialized(true));
    }, []);

    // Listen for host ticket creation events
    useEffect(() => {
        const handleTicketCreated = (event: CustomEvent) => {
            const data = JSON.parse(event.detail);
            setHostTicket(data.ticket);
        };

        window.addEventListener("host-ticket-created", handleTicketCreated as EventListener);
        return () => {
            window.removeEventListener("host-ticket-created", handleTicketCreated as EventListener);
        };
    }, []);

    // Original local connection method
    const connect = useCallback(
        (roomId: string) => {
            if (isConnected || !isInitialized) return;

            console.log("Starting P2P manager for room:", roomId);
            const p2pHandle = start_p2p_manager(roomId);
            setHandle(p2pHandle);
            setIsConnected(true);
            setConnectionMode("local");
        },
        [isConnected, isInitialized],
    );

    // New serverless methods
    const createHost = useCallback(() => {
        if (isConnected || !isInitialized) return;

        console.log("Creating serverless host");
        try {
            const p2pHandle = create_serverless_host();
            setHandle(p2pHandle);
            setIsConnected(true);
            setConnectionMode("serverless");
        } catch (error) {
            console.error("Failed to create serverless host:", error);
        }
    }, [isConnected, isInitialized]);

    const joinWithTicketString = useCallback(
        (ticket: string) => {
            if (isConnected || !isInitialized) return;

            console.log("Joining with ticket:", ticket);
            try {
                const p2pHandle = join_with_ticket(ticket);
                setHandle(p2pHandle);
                setIsConnected(true);
                setConnectionMode("serverless");
            } catch (error) {
                console.error("Failed to join with ticket:", error);
            }
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

    const disconnect = useCallback(() => {
        if (handle) {
            handle.free();
            setHandle(null);
        }
        setIsConnected(false);
        setHostTicket(null);
    }, [handle]);

    return {
        isInitialized,
        isConnected,
        connectionMode,
        hostTicket,
        receivedMessages,
        // Local mode
        connect,
        // Serverless mode
        createHost,
        joinWithTicket: joinWithTicketString,
        // Common
        sendMessage,
        disconnect,
    };
}
