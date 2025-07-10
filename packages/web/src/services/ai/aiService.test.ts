import { beforeEach, describe, expect, it, vi } from "vitest";
import { type AIService, createAIService } from "./aiService";

// Mock Worker
class MockWorker {
    onmessage: ((event: MessageEvent) => void) | null = null;
    private listeners = new Map<string, Set<(event: MessageEvent) => void>>();

    addEventListener(type: string, listener: (event: MessageEvent) => void): void {
        if (!this.listeners.has(type)) {
            this.listeners.set(type, new Set());
        }
        this.listeners.get(type)?.add(listener);
    }

    removeEventListener(type: string, listener: (event: MessageEvent) => void): void {
        this.listeners.get(type)?.delete(listener);
    }

    postMessage(data: unknown): void {
        // Mock implementation
    }

    terminate(): void {
        // Mock implementation
    }

    dispatchEvent(event: MessageEvent): void {
        const listeners = this.listeners.get(event.type);
        if (listeners) {
            for (const listener of listeners) {
                listener(event);
            }
        }
    }
}

// Mock the Worker constructor
vi.stubGlobal("Worker", MockWorker);

describe("AIService", () => {
    let aiService: AIService;

    beforeEach(() => {
        vi.clearAllMocks();
    });

    describe("createAIService", () => {
        it("should create an AI service with default difficulty", () => {
            aiService = createAIService();
            const player = aiService.getAIPlayer();

            expect(player.difficulty).toBe("intermediate");
            expect(player.name).toBe("AI中級者");
            expect(player.isThinking).toBe(false);
            expect(player.id).toMatch(/^ai-\d+-\d+$/);
        });

        it("should create an AI service with specified difficulty", () => {
            aiService = createAIService("expert");
            const player = aiService.getAIPlayer();

            expect(player.difficulty).toBe("expert");
            expect(player.name).toBe("AIエキスパート");
        });

        it("should create an AI service with custom name", () => {
            aiService = createAIService("beginner", "カスタムAI");
            const player = aiService.getAIPlayer();

            expect(player.name).toBe("カスタムAI");
        });
    });

    describe("Multiple instances", () => {
        it("should create independent instances", () => {
            const service1 = createAIService("beginner");
            const service2 = createAIService("expert");

            const player1 = service1.getAIPlayer();
            const player2 = service2.getAIPlayer();

            expect(player1.difficulty).toBe("beginner");
            expect(player2.difficulty).toBe("expert");
            expect(player1.id).not.toBe(player2.id);
        });
    });

    describe("getPlayer", () => {
        it("should return the same player as getAIPlayer", () => {
            aiService = createAIService();
            const player1 = aiService.getAIPlayer();
            const player2 = aiService.getPlayer();

            expect(player1).toEqual(player2);
        });
    });

    describe("isThinking", () => {
        it("should return false initially", () => {
            aiService = createAIService();
            expect(aiService.isThinking()).toBe(false);
        });
    });

    describe("cleanup and dispose", () => {
        it("should cleanup resources when dispose is called", () => {
            aiService = createAIService();

            // Dispose should not throw
            expect(() => aiService.dispose()).not.toThrow();
        });

        it("should cleanup resources when cleanup is called", () => {
            aiService = createAIService();

            // Cleanup should not throw
            expect(() => aiService.cleanup()).not.toThrow();
        });
    });
});
