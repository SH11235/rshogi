import type { AudioManager, SoundType } from "@/types/audio";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { createBrowserAudioManager } from "./audioManager";

// モックaudioGenerator
vi.mock("./audioGenerator", () => ({
    generateSoundForType: vi.fn(() =>
        Promise.resolve(new Blob(["mock audio"], { type: "audio/wav" })),
    ),
    createAudioBlobURL: vi.fn((blob: Blob) => `blob:mock-url-${Math.random()}`),
    revokeBlobURL: vi.fn(),
}));

// HTMLAudioElementのモック
class MockHTMLAudioElement {
    src = "";
    volume = 1;
    playbackRate = 1;
    currentTime = 0;
    paused = true;
    ended = false;
    readyState = 4; // HAVE_ENOUGH_DATA
    preload = "metadata";

    play = vi.fn(() => Promise.resolve());
    pause = vi.fn(() => {
        this.paused = true;
    });
    load = vi.fn();
    addEventListener = vi.fn((event: string, handler: () => void) => {
        if (event === "canplaythrough") {
            // 即座にロード完了をシミュレート
            setTimeout(handler, 0);
        }
    });
    removeEventListener = vi.fn();
}

// グローバルAudioコンストラクタのモック
const mockAudioConstructor = vi.fn(() => new MockHTMLAudioElement());
global.Audio = mockAudioConstructor as unknown as typeof Audio;

describe("createBrowserAudioManager", () => {
    let audioManager: AudioManager;

    beforeEach(() => {
        vi.clearAllMocks();
        audioManager = createBrowserAudioManager();
    });

    describe("initialize", () => {
        it("音声マネージャーを初期化できる", async () => {
            await audioManager.initialize();

            const state = audioManager.getState();
            expect(state.isInitialized).toBe(true);
        });

        it("二回目の初期化は何もしない", async () => {
            await audioManager.initialize();
            const firstState = audioManager.getState();

            await audioManager.initialize();
            const secondState = audioManager.getState();

            expect(firstState).toEqual(secondState);
        });

        it("Audio APIが利用できない場合でも初期化される", async () => {
            // @ts-expect-error 意図的にundefinedを設定
            global.Audio = undefined;

            await audioManager.initialize();

            const state = audioManager.getState();
            expect(state.isInitialized).toBe(true);

            // グローバルAudioを復元
            global.Audio = MockHTMLAudioElement as unknown as typeof Audio;
        });
    });

    describe("play", () => {
        beforeEach(async () => {
            vi.clearAllMocks();
            await audioManager.initialize();
        });

        it("音声を再生できる", async () => {
            // エラーなく音声を再生できることを確認
            await expect(audioManager.play("piece")).resolves.toBeUndefined();
        });

        it("ミュート中は再生されない", async () => {
            const mockAudio = new MockHTMLAudioElement();
            mockAudioConstructor.mockReturnValue(mockAudio);

            audioManager.setMuted(true);
            await audioManager.play("piece");

            expect(mockAudio.play).not.toHaveBeenCalled();
        });

        it("未初期化では再生されない", async () => {
            audioManager = createBrowserAudioManager(); // 新しいインスタンスで未初期化状態
            const mockAudio = new MockHTMLAudioElement();
            mockAudioConstructor.mockReturnValue(mockAudio);

            await audioManager.play("piece");

            expect(mockAudio.play).not.toHaveBeenCalled();
        });

        it("カスタム音量で再生できる", async () => {
            // エラーなくカスタム音量で再生できることを確認
            await expect(audioManager.play("piece", { volume: 0.8 })).resolves.toBeUndefined();
        });

        it("再生速度を設定できる", async () => {
            // エラーなく再生速度を設定して再生できることを確認
            await expect(
                audioManager.play("piece", { playbackRate: 1.5 }),
            ).resolves.toBeUndefined();
        });

        it("interrupt オプションで現在の再生を停止できる", async () => {
            // 最初に音声を再生
            await audioManager.play("piece");

            // interruptオプションで同じ音声を再生
            await expect(audioManager.play("piece", { interrupt: true })).resolves.toBeUndefined();
        });

        it("不明な音声タイプは警告を出すが例外は投げない", async () => {
            const consoleWarnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});

            await audioManager.play("unknown" as SoundType);

            expect(consoleWarnSpy).toHaveBeenCalledWith("Unknown sound type: unknown");
            consoleWarnSpy.mockRestore();
        });
    });

    describe("volume control", () => {
        it("音量を設定できる", () => {
            audioManager.setVolume(75);

            const state = audioManager.getState();
            expect(state.volume).toBe(75);
        });
    });

    describe("mute control", () => {
        it("ミュート状態を設定できる", () => {
            audioManager.setMuted(true);

            const state = audioManager.getState();
            expect(state.isMuted).toBe(true);
        });

        it("ミュート解除できる", () => {
            audioManager.setMuted(true);
            audioManager.setMuted(false);

            const state = audioManager.getState();
            expect(state.isMuted).toBe(false);
        });
    });

    describe("preload", () => {
        it("音声をプリロードできる", async () => {
            await audioManager.initialize();
            await audioManager.preload("check");

            const state = audioManager.getState();
            expect(state.loadedSounds.has("check")).toBe(true);
        });

        it("不明な音声タイプのプリロードはエラーを投げる", async () => {
            await expect(audioManager.preload("unknown" as SoundType)).rejects.toThrow(
                "Unknown sound type: unknown",
            );
        });
    });

    describe("stopAll", () => {
        it("すべての音声を停止できる", async () => {
            await audioManager.initialize();

            const mockAudios: MockHTMLAudioElement[] = [];
            mockAudioConstructor.mockImplementation(() => {
                const audio = new MockHTMLAudioElement();
                mockAudios.push(audio);
                return audio;
            });

            // 複数の音声を再生
            await audioManager.play("piece");
            await audioManager.play("check");

            audioManager.stopAll();

            for (const audio of mockAudios) {
                expect(audio.pause).toHaveBeenCalled();
                expect(audio.currentTime).toBe(0);
            }
        });
    });

    describe("getState", () => {
        it("現在の状態を取得できる", async () => {
            await audioManager.initialize();
            audioManager.setVolume(80);
            audioManager.setMuted(true);
            await audioManager.preload("gameEnd");

            const state = audioManager.getState();

            expect(state).toEqual({
                isInitialized: true,
                volume: 80,
                isMuted: true,
                loadedSounds: new Set(["piece", "check", "gameEnd"]), // preload対象 + gameEnd
            });
        });

        it("loadedSoundsは新しいSetインスタンスを返す", () => {
            const state1 = audioManager.getState();
            const state2 = audioManager.getState();

            expect(state1.loadedSounds).not.toBe(state2.loadedSounds);
            expect(state1.loadedSounds).toEqual(state2.loadedSounds);
        });
    });
});
