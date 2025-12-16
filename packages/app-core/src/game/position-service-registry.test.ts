import { afterEach, describe, expect, it, vi } from "vitest";
import type { PositionService } from "./position-service";
import { getPositionService, setPositionServiceFactory } from "./position-service-registry";

describe("position-service-registry", () => {
    // テスト後にクリーンアップ
    afterEach(() => {
        vi.clearAllMocks();
    });

    describe("setPositionServiceFactory と getPositionService", () => {
        it("factory を設定してサービスを取得できる", () => {
            const mockService: PositionService = {
                async getInitialBoard() {
                    return {
                        board: {} as Record<string, never>,
                        hands: { sente: {}, gote: {} },
                        turn: "sente",
                    };
                },
                async parseSfen(_sfen: string) {
                    return {
                        board: {} as Record<string, never>,
                        hands: { sente: {}, gote: {} },
                        turn: "sente",
                    };
                },
                async boardToSfen(_position) {
                    return "startpos";
                },
                async getLegalMoves(_sfen: string, _moves?: string[]) {
                    return [];
                },
                async replayMovesStrict(_sfen: string, moves: string[]) {
                    return {
                        applied: moves,
                        lastPly: moves.length,
                        position: {
                            board: {} as Record<string, never>,
                            hands: { sente: {}, gote: {} },
                            turn: "sente",
                        },
                    };
                },
            };

            const factory = vi.fn(() => mockService);
            setPositionServiceFactory(factory);

            const service = getPositionService();

            expect(service).toBe(mockService);
            expect(factory).toHaveBeenCalledTimes(1);
        });

        it("サービスはキャッシュされる", () => {
            const mockService: PositionService = {
                async getInitialBoard() {
                    return {
                        board: {} as Record<string, never>,
                        hands: { sente: {}, gote: {} },
                        turn: "sente",
                    };
                },
                async parseSfen(_sfen: string) {
                    return {
                        board: {} as Record<string, never>,
                        hands: { sente: {}, gote: {} },
                        turn: "sente",
                    };
                },
                async boardToSfen(_position) {
                    return "startpos";
                },
                async getLegalMoves(_sfen: string, _moves?: string[]) {
                    return [];
                },
                async replayMovesStrict(_sfen: string, moves: string[]) {
                    return {
                        applied: moves,
                        lastPly: moves.length,
                        position: {
                            board: {} as Record<string, never>,
                            hands: { sente: {}, gote: {} },
                            turn: "sente",
                        },
                    };
                },
            };

            const factory = vi.fn(() => mockService);
            setPositionServiceFactory(factory);

            const service1 = getPositionService();
            const service2 = getPositionService();

            // 同じインスタンスが返される
            expect(service1).toBe(service2);
            // factory は最初の1回しか呼ばれない
            expect(factory).toHaveBeenCalledTimes(1);
        });

        it("factory を再設定するとキャッシュがクリアされる", () => {
            const mockService1: PositionService = {
                async getInitialBoard() {
                    return {
                        board: {} as Record<string, never>,
                        hands: { sente: {}, gote: {} },
                        turn: "sente",
                    };
                },
                async parseSfen(_sfen: string) {
                    return {
                        board: {} as Record<string, never>,
                        hands: { sente: {}, gote: {} },
                        turn: "sente",
                    };
                },
                async boardToSfen(_position) {
                    return "startpos1";
                },
                async getLegalMoves(_sfen: string, _moves?: string[]) {
                    return [];
                },
                async replayMovesStrict(_sfen: string, moves: string[]) {
                    return {
                        applied: moves,
                        lastPly: moves.length,
                        position: {
                            board: {} as Record<string, never>,
                            hands: { sente: {}, gote: {} },
                            turn: "sente",
                        },
                    };
                },
            };

            const mockService2: PositionService = {
                async getInitialBoard() {
                    return {
                        board: {} as Record<string, never>,
                        hands: { sente: {}, gote: {} },
                        turn: "sente",
                    };
                },
                async parseSfen(_sfen: string) {
                    return {
                        board: {} as Record<string, never>,
                        hands: { sente: {}, gote: {} },
                        turn: "sente",
                    };
                },
                async boardToSfen(_position) {
                    return "startpos2";
                },
                async getLegalMoves(_sfen: string, _moves?: string[]) {
                    return [];
                },
                async replayMovesStrict(_sfen: string, moves: string[]) {
                    return {
                        applied: moves,
                        lastPly: moves.length,
                        position: {
                            board: {} as Record<string, never>,
                            hands: { sente: {}, gote: {} },
                            turn: "sente",
                        },
                    };
                },
            };

            const factory1 = vi.fn(() => mockService1);
            const factory2 = vi.fn(() => mockService2);

            setPositionServiceFactory(factory1);
            const service1 = getPositionService();

            expect(service1).toBe(mockService1);

            // factory を再設定
            setPositionServiceFactory(factory2);
            const service2 = getPositionService();

            // 新しいサービスが返される
            expect(service2).toBe(mockService2);
            expect(service2).not.toBe(service1);

            // 各 factory は1回ずつ呼ばれる
            expect(factory1).toHaveBeenCalledTimes(1);
            expect(factory2).toHaveBeenCalledTimes(1);
        });
    });
});
