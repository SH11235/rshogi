# WebRTC Implementation Patterns

This document describes best practices and patterns for implementing WebRTC communication in the Shogi application.

## Table of Contents
1. [Connection Lifecycle](#connection-lifecycle)
2. [Error Handling](#error-handling)
3. [Message Protocol](#message-protocol)
4. [Testing Strategies](#testing-strategies)

## Connection Lifecycle

### Initialization
```typescript
class WebRTCConnection {
    private peer: RTCPeerConnection | null = null;
    private dataChannel: RTCDataChannel | null = null;
    
    async initialize(): Promise<void> {
        // Load WASM module first
        await this.loadWasmModule();
        
        // Create peer connection with proper config
        this.peer = new RTCPeerConnection({
            iceServers: [{ urls: 'stun:stun.l.google.com:19302' }]
        });
        
        // Set up event handlers
        this.setupEventHandlers();
    }
}
```

### Connection States
- `new`: Initial state
- `connecting`: Establishing connection
- `connected`: Active connection
- `disconnected`: Connection lost
- `failed`: Connection failed

### State Transitions
```typescript
private updateConnectionState(state: ConnectionState): void {
    this.connectionState = state;
    this.onConnectionStateChange?.(state);
    
    // Handle specific state transitions
    switch (state) {
        case 'disconnected':
            this.scheduleReconnect();
            break;
        case 'failed':
            this.cleanup();
            break;
    }
}
```

## Error Handling

### Error Types
```typescript
export class WebRTCError extends Error {
    constructor(
        message: string,
        public code: 'CONNECTION_FAILED' | 'SEND_FAILED' | 'NOT_CONNECTED' | 'MAX_ATTEMPTS'
    ) {
        super(message);
        this.name = 'WebRTCError';
    }
}
```

### Reconnection Strategy
1. **Exponential Backoff**: Start with 2s, double each attempt, max 30s
2. **Max Attempts**: Default 5 attempts
3. **State Preservation**: Maintain game state during reconnection
4. **User Feedback**: Show connection status to users

```typescript
private async scheduleReconnect(): Promise<void> {
    if (this.reconnectAttempt >= this.MAX_RECONNECT_ATTEMPTS) {
        this.handleError(new WebRTCError('Max reconnection attempts reached', 'MAX_ATTEMPTS'));
        return;
    }
    
    const delay = Math.min(2000 * Math.pow(2, this.reconnectAttempt), 30000);
    console.log(`Scheduling reconnect attempt ${this.reconnectAttempt + 1} in ${delay}ms`);
    
    this.reconnectTimer = setTimeout(() => {
        this.attemptReconnect();
    }, delay);
}
```

### Error Recovery
- **Automatic Recovery**: For transient errors
- **Manual Recovery**: For persistent errors
- **State Sync**: After successful reconnection
- **Graceful Degradation**: Fallback to local play

## Message Protocol

### Message Structure
```typescript
interface GameMessage {
    type: string;
    data: unknown;
    timestamp: number;
    playerId: string;
}
```

### Message Types
1. **Game Messages**: Move, resign, draw offer
2. **Timer Messages**: Timer config, timer update
3. **State Messages**: Sync state, game start
4. **Special Messages**: Repetition check, jishogi check
5. **Spectator Messages**: Join, leave, sync

### Type Guards
```typescript
export function isMoveMessage(msg: GameMessage): msg is MoveMessage {
    return msg.type === 'move' && 
           'from' in msg.data && 
           'to' in msg.data;
}
```

### Message Validation
```typescript
private validateMessage(message: unknown): GameMessage {
    if (!isObject(message)) {
        throw new Error('Invalid message format');
    }
    
    if (!('type' in message) || typeof message.type !== 'string') {
        throw new Error('Message missing type');
    }
    
    if (!('timestamp' in message) || typeof message.timestamp !== 'number') {
        throw new Error('Message missing timestamp');
    }
    
    return message as GameMessage;
}
```

## Testing Strategies

### Mocking WebRTC
```typescript
// Mock RTCPeerConnection
vi.mock('webrtc', () => ({
    RTCPeerConnection: vi.fn(() => ({
        createOffer: vi.fn(),
        createAnswer: vi.fn(),
        setLocalDescription: vi.fn(),
        setRemoteDescription: vi.fn(),
        close: vi.fn()
    })),
    RTCDataChannel: vi.fn()
}));
```

### Testing Connection States
```typescript
it('should handle connection lifecycle', async () => {
    const connection = new WebRTCConnection();
    const stateChanges: ConnectionState[] = [];
    
    connection.onConnectionStateChange((state) => {
        stateChanges.push(state);
    });
    
    await connection.createHost();
    expect(stateChanges).toContain('connecting');
    
    // Simulate connection
    await connection.simulateConnection();
    expect(stateChanges).toContain('connected');
});
```

### Testing Reconnection
```typescript
it('should reconnect with exponential backoff', async () => {
    vi.useFakeTimers();
    const connection = new WebRTCConnection();
    
    // Simulate disconnection
    connection.simulateDisconnect();
    
    // First attempt after 2s
    vi.advanceTimersByTime(2000);
    expect(connection.reconnectAttempt).toBe(1);
    
    // Second attempt after 4s
    vi.advanceTimersByTime(4000);
    expect(connection.reconnectAttempt).toBe(2);
});
```

### Testing Error Scenarios
```typescript
it('should handle send failures gracefully', async () => {
    const connection = new WebRTCConnection();
    const errorHandler = vi.fn();
    
    connection.onError(errorHandler);
    
    // Try to send without connection
    connection.sendMessage({ type: 'move', data: {} });
    
    expect(errorHandler).toHaveBeenCalledWith(
        expect.objectContaining({
            code: 'NOT_CONNECTED'
        })
    );
});
```

## Best Practices

### Do's
- ✅ Always validate incoming messages
- ✅ Implement proper cleanup on disconnect
- ✅ Use type guards for message handling
- ✅ Provide user feedback for connection states
- ✅ Test error scenarios thoroughly

### Don'ts
- ❌ Don't assume connection is always available
- ❌ Don't send sensitive data without validation
- ❌ Don't ignore error states
- ❌ Don't block UI during reconnection
- ❌ Don't leak resources (timers, connections)

## Common Pitfalls

1. **Race Conditions**: Messages arriving out of order
   - Solution: Use timestamps and sequence numbers

2. **Memory Leaks**: Not cleaning up event listeners
   - Solution: Implement proper cleanup methods

3. **Infinite Loops**: Reconnection without limits
   - Solution: Implement max attempts and backoff

4. **State Desync**: Local and remote state mismatch
   - Solution: Periodic full state synchronization

5. **Type Safety**: Using any types for messages
   - Solution: Define strict types and use type guards