/**
 * 開発用音声ファイル生成ユーティリティ
 * 実際の音声ファイルが準備されるまでのプレースホルダー
 */

import { audioLogger } from "./audioLogger";

/**
 * Web Audio APIを使って短いビープ音を生成
 */
export function generateBeep(frequency = 440, duration = 0.2, volume = 0.3): Promise<Blob> {
    audioLogger.debug("AudioGenerator", "Creating beep", { frequency, duration, volume });

    return new Promise((resolve, reject) => {
        try {
            // Web Audio APIの利用可能性チェック
            const windowWithWebkit = window as typeof window & {
                webkitAudioContext?: typeof AudioContext;
            };

            if (
                typeof AudioContext === "undefined" &&
                typeof windowWithWebkit.webkitAudioContext === "undefined"
            ) {
                throw new Error("Web Audio API not supported");
            }

            const AudioContextClass = AudioContext || windowWithWebkit.webkitAudioContext;
            if (!AudioContextClass) {
                throw new Error("AudioContext not available");
            }
            const audioContext = new AudioContextClass();

            const sampleRate = audioContext.sampleRate;
            const numSamples = Math.floor(sampleRate * duration);
            const buffer = audioContext.createBuffer(1, numSamples, sampleRate);
            const channelData = buffer.getChannelData(0);

            // シンプルなサイン波を生成
            for (let i = 0; i < numSamples; i++) {
                const t = i / sampleRate;
                channelData[i] = Math.sin(2 * Math.PI * frequency * t) * volume;
            }

            // フェードイン・フェードアウト適用
            const fadeLength = Math.floor(sampleRate * 0.01); // 10ms
            for (let i = 0; i < fadeLength; i++) {
                const factor = i / fadeLength;
                channelData[i] *= factor;
                channelData[numSamples - 1 - i] *= factor;
            }

            // WAVファイルとしてエンコード
            const wavBlob = encodeWAV(buffer);
            audioLogger.debug("AudioGenerator", "Generated WAV blob", { size: wavBlob.size });
            resolve(wavBlob);
        } catch (error) {
            const audioError = error instanceof Error ? error : new Error(String(error));
            audioLogger.error("AudioGenerator", "Failed to generate beep", {
                frequency,
                duration,
                volume,
                error: audioError.message,
            });
            reject(audioError);
        }
    });
}

/**
 * AudioBufferをWAVファイルにエンコード
 */
function encodeWAV(buffer: AudioBuffer): Blob {
    const length = buffer.length;
    const sampleRate = buffer.sampleRate;
    const numChannels = buffer.numberOfChannels;
    const bytesPerSample = 2;

    const arrayBuffer = new ArrayBuffer(44 + length * numChannels * bytesPerSample);
    const view = new DataView(arrayBuffer);

    // WAVヘッダー
    writeString(view, 0, "RIFF");
    view.setUint32(4, 36 + length * numChannels * bytesPerSample, true);
    writeString(view, 8, "WAVE");
    writeString(view, 12, "fmt ");
    view.setUint32(16, 16, true); // サブチャンクサイズ
    view.setUint16(20, 1, true); // オーディオフォーマット（PCM）
    view.setUint16(22, numChannels, true);
    view.setUint32(24, sampleRate, true);
    view.setUint32(28, sampleRate * numChannels * bytesPerSample, true);
    view.setUint16(32, numChannels * bytesPerSample, true);
    view.setUint16(34, 8 * bytesPerSample, true);
    writeString(view, 36, "data");
    view.setUint32(40, length * numChannels * bytesPerSample, true);

    // 音声データ
    let offset = 44;
    for (let i = 0; i < length; i++) {
        for (let channel = 0; channel < numChannels; channel++) {
            const sample = Math.max(-1, Math.min(1, buffer.getChannelData(channel)[i]));
            view.setInt16(offset, sample * 0x7fff, true);
            offset += 2;
        }
    }

    return new Blob([arrayBuffer], { type: "audio/wav" });
}

function writeString(view: DataView, offset: number, string: string): void {
    for (let i = 0; i < string.length; i++) {
        view.setUint8(offset + i, string.charCodeAt(i));
    }
}

/**
 * 音声タイプに応じた特徴的なビープ音を生成
 */
export async function generateSoundForType(type: "piece" | "check" | "gameEnd"): Promise<Blob> {
    audioLogger.debug("AudioGenerator", "Generating sound for type", { type });

    try {
        let blob: Blob;
        switch (type) {
            case "piece":
                // 駒音: 短いクリック音（高周波数）
                blob = await generateBeep(800, 0.1, 0.2);
                break;

            case "check":
                // 王手音: 警告音（中周波数、少し長め）
                blob = await generateBeep(600, 0.3, 0.4);
                break;

            case "gameEnd":
                // ゲーム終了音: 終了チャイム（低周波数、長め）
                blob = await generateBeep(400, 0.5, 0.3);
                break;

            default:
                blob = await generateBeep();
        }

        audioLogger.debug("AudioGenerator", "Successfully generated sound", {
            type,
            size: blob.size,
        });
        return blob;
    } catch (error) {
        const audioError = error instanceof Error ? error : new Error(String(error));
        audioLogger.error("AudioGenerator", "Failed to generate sound", {
            type,
            error: audioError.message,
        });
        throw audioError;
    }
}

/**
 * 生成した音声をBlobURLとして作成
 */
export function createAudioBlobURL(blob: Blob): string {
    return URL.createObjectURL(blob);
}

/**
 * BlobURLを解放
 */
export function revokeBlobURL(url: string): void {
    URL.revokeObjectURL(url);
}
