# WebRTC Message Types Extension Guide

This document explains how to extend message types for WebRTC communication in the Shogi application.

## Current Message Types

### Base Message Interface
```typescript
export interface GameMessage {
    type: string;
    data: unknown;
    timestamp: number;
    playerId: string;
}
```

### Existing Message Types
1. `move` - Player moves
2. `resign` - Resignation
3. `draw_offer` - Draw proposals
4. `game_start` - Game initialization
5. `timer_config` - Timer settings
6. `timer_update` - Timer updates
7. `repetition_check` - Sennichite detection
8. `jishogi_check` - Impasse detection

## Adding New Message Types

### Step 1: Define the Message Interface
```typescript
// In types/online.ts
export interface YourFeatureMessage extends GameMessage {
    type: "your_feature";
    data: {
        // Your specific data fields
        fieldName: string;
        fieldValue: number;
        // ... more fields
    };
}
```

### Step 2: Add to Union Type
```typescript
// Update the type field in GameMessage
export interface GameMessage {
    type:
        | "move"
        | "resign"
        // ... existing types
        | "your_feature"; // Add your new type
    data: unknown;
    timestamp: number;
    playerId: string;
}
```

### Step 3: Create Type Guard
```typescript
export function isYourFeatureMessage(msg: GameMessage): msg is YourFeatureMessage {
    return msg.type === "your_feature";
}
```

### Step 4: Add Message Handler
```typescript
// In gameStore.ts handleOnlineMessage
handleOnlineMessage: (message: GameMessage) => {
    // ... existing handlers
    
    if (isYourFeatureMessage(message)) {
        // Handle your feature message
        const { data } = message;
        set({
            // Update state based on message
            yourFeatureState: data.fieldValue
        });
        
        console.log("Your feature message received:", data);
    }
}
```

### Step 5: Create Message Sender
```typescript
// In gameStore.ts or as a separate action
sendYourFeatureMessage: (fieldValue: number) => {
    const { webrtcConnection } = get();
    if (!webrtcConnection) return;
    
    const message: YourFeatureMessage = {
        type: "your_feature",
        data: {
            fieldName: "example",
            fieldValue
        },
        timestamp: Date.now(),
        playerId: webrtcConnection.getConnectionInfo().peerId
    };
    
    webrtcConnection.sendMessage(message);
}
```

## Example: Adding Chat Feature

### 1. Define Chat Message
```typescript
export interface ChatMessage extends GameMessage {
    type: "chat";
    data: {
        text: string;
        sender: Player;
    };
}
```

### 2. Create Type Guard
```typescript
export function isChatMessage(msg: GameMessage): msg is ChatMessage {
    return msg.type === "chat" && 
           typeof msg.data === "object" &&
           "text" in msg.data &&
           "sender" in msg.data;
}
```

### 3. Add to Store
```typescript
interface ChatState {
    chatHistory: Array<{
        text: string;
        sender: Player;
        timestamp: number;
    }>;
    
    sendChatMessage: (text: string) => void;
}
```

### 4. Implement Handler
```typescript
if (isChatMessage(message)) {
    const { data } = message;
    const { chatHistory } = get();
    
    set({
        chatHistory: [...chatHistory, {
            text: data.text,
            sender: data.sender,
            timestamp: message.timestamp
        }]
    });
}
```

### 5. Implement Sender
```typescript
sendChatMessage: (text: string) => {
    const { webrtcConnection, localPlayer } = get();
    if (!webrtcConnection || !localPlayer) return;
    
    const message: ChatMessage = {
        type: "chat",
        data: {
            text,
            sender: localPlayer
        },
        timestamp: Date.now(),
        playerId: webrtcConnection.getConnectionInfo().peerId
    };
    
    webrtcConnection.sendMessage(message);
    
    // Also update local state
    const { chatHistory } = get();
    set({
        chatHistory: [...chatHistory, {
            text,
            sender: localPlayer,
            timestamp: Date.now()
        }]
    });
}
```

## Message Validation

### Basic Validation
```typescript
function validateMessage(message: unknown): message is GameMessage {
    if (!message || typeof message !== "object") return false;
    
    const msg = message as any;
    return (
        typeof msg.type === "string" &&
        "data" in msg &&
        typeof msg.timestamp === "number" &&
        typeof msg.playerId === "string"
    );
}
```

### Data-Specific Validation
```typescript
function validateChatData(data: unknown): data is ChatMessage["data"] {
    if (!data || typeof data !== "object") return false;
    
    const chatData = data as any;
    return (
        typeof chatData.text === "string" &&
        chatData.text.length > 0 &&
        chatData.text.length <= 500 && // Max length
        (chatData.sender === "black" || chatData.sender === "white")
    );
}
```

## Best Practices

### 1. Type Safety
- Always define strict TypeScript interfaces
- Use discriminated unions for message types
- Create comprehensive type guards

### 2. Validation
- Validate all incoming messages
- Check data constraints (length, range, etc.)
- Handle malformed messages gracefully

### 3. State Updates
- Keep message handlers pure
- Update state atomically
- Consider race conditions

### 4. Documentation
- Document message format
- Include examples
- Specify constraints

### 5. Testing
```typescript
describe("Chat message handling", () => {
    it("should handle chat messages", () => {
        const store = useGameStore.getState();
        
        const chatMessage: ChatMessage = {
            type: "chat",
            data: {
                text: "Good game!",
                sender: "black"
            },
            timestamp: Date.now(),
            playerId: "test-id"
        };
        
        store.handleOnlineMessage(chatMessage);
        
        expect(store.chatHistory).toHaveLength(1);
        expect(store.chatHistory[0].text).toBe("Good game!");
    });
});
```

## Message Flow Diagram

```
Player A                    WebRTC                    Player B
   |                          |                          |
   |-- Create Message ------->|                          |
   |                          |                          |
   |                          |-- Send via DataChannel ->|
   |                          |                          |
   |                          |                          |-- Validate Message
   |                          |                          |
   |                          |                          |-- Update State
   |                          |                          |
   |<- State Update ----------|<- Acknowledgment -------|
```

## Troubleshooting

### Common Issues

1. **Message not received**
   - Check type guard implementation
   - Verify message type is in union
   - Check connection status

2. **Type errors**
   - Ensure interface extends GameMessage
   - Check data field types
   - Verify type guard return type

3. **State not updating**
   - Check handler implementation
   - Verify state setter usage
   - Look for race conditions

### Debug Helpers
```typescript
// Log all messages
handleOnlineMessage: (message: GameMessage) => {
    console.log("Received message:", message);
    
    // ... handle message
}

// Type checking helper
function debugMessageType(message: GameMessage): void {
    console.log(`Message type: ${message.type}`);
    console.log(`Type guard results:`);
    console.log(`  isMoveMessage: ${isMoveMessage(message)}`);
    console.log(`  isChatMessage: ${isChatMessage(message)}`);
    // ... check all type guards
}
```