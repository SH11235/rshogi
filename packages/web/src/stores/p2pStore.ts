let receivedMessages: string[] = [];
let listeners: (() => void)[] = [];

const handleP2PMessage = (event: Event) => {
    const customEvent = event as CustomEvent<string>;
    receivedMessages = [...receivedMessages, customEvent.detail];
    emitChange();
};

if (typeof window !== "undefined") {
    window.addEventListener("p2p-message", handleP2PMessage);
}

function emitChange() {
    for (const listener of listeners) {
        listener();
    }
}

export const p2pStore = {
    subscribe(listener: () => void) {
        listeners = [...listeners, listener];
        return () => {
            listeners = listeners.filter((l) => l !== listener);
        };
    },
    getSnapshot() {
        return receivedMessages;
    },
};
