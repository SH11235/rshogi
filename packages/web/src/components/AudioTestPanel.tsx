import { useAudio, useAudioInitializer } from "@/hooks/useAudio";
import { useGameSettings } from "@/hooks/useGameSettings";
import type { SoundType } from "@/types/audio";
import { Button } from "./ui/button";

/**
 * 開発・テスト用の音声パネル
 * 本番では表示されない
 */
export function AudioTestPanel() {
    const { playForced, state, isReady } = useAudio();
    const { hasUserInteracted, initializeOnInteraction } = useAudioInitializer();
    const { settings } = useGameSettings();

    // 本番環境では表示しない
    if (process.env.NODE_ENV === "production") {
        return null;
    }

    const handlePlay = async (type: SoundType) => {
        console.log(`AudioTestPanel: Attempting to play ${type}`);
        console.log(`AudioTestPanel: isReady=${isReady}, hasUserInteracted=${hasUserInteracted}`);

        if (!hasUserInteracted) {
            console.log("AudioTestPanel: Initializing audio on first interaction");
            await initializeOnInteraction();
        }

        try {
            await playForced(type);
            console.log(`AudioTestPanel: Successfully triggered playForced for ${type}`);
        } catch (error) {
            console.error(`AudioTestPanel: Failed to play ${type}:`, error);
        }
    };

    return (
        <div className="p-4 border rounded-lg bg-yellow-50 border-yellow-200">
            <h3 className="text-lg font-semibold mb-3">🔊 音声テスト（開発用）</h3>

            {/* 状態表示 */}
            <div className="mb-4 space-y-1 text-sm">
                <div>初期化済み: {isReady ? "✅" : "❌"}</div>
                <div>ユーザー操作済み: {hasUserInteracted ? "✅" : "❌"}</div>
                <div>音量: {state.volume}%</div>
                <div>ミュート: {state.isMuted ? "🔇" : "🔊"}</div>
                <div>読み込み済み: {Array.from(state.loadedSounds).join(", ") || "なし"}</div>

                {/* 設定状態 */}
                <div className="border-t pt-2 mt-2">
                    <div className="font-semibold">音声設定:</div>
                    <div>マスター音量: {settings.audio.masterVolume}%</div>
                    <div>駒音: {settings.audio.pieceSound ? "有効" : "無効"}</div>
                    <div>王手音: {settings.audio.checkSound ? "有効" : "無効"}</div>
                    <div>終了音: {settings.audio.gameEndSound ? "有効" : "無効"}</div>
                </div>
            </div>

            {/* 音声テストボタン */}
            <div className="space-y-2">
                <div className="text-sm text-gray-600 mb-2">
                    💡 このテストパネルは設定に関係なく音声を再生します
                </div>
                <div className="flex gap-2">
                    <Button
                        type="button"
                        size="sm"
                        onClick={() => handlePlay("piece")}
                        disabled={!isReady}
                    >
                        駒音
                    </Button>
                    <Button
                        type="button"
                        size="sm"
                        onClick={() => handlePlay("check")}
                        disabled={!isReady}
                    >
                        王手音
                    </Button>
                    <Button
                        type="button"
                        size="sm"
                        onClick={() => handlePlay("gameEnd")}
                        disabled={!isReady}
                    >
                        ゲーム終了音
                    </Button>
                </div>

                {!hasUserInteracted && (
                    <div className="text-xs text-yellow-700">
                        💡 ヒント: ブラウザの自動再生ポリシーにより、最初のクリックが必要です
                    </div>
                )}
            </div>
        </div>
    );
}
