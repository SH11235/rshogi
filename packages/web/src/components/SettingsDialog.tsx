import { useGameSettings } from "@/hooks/useGameSettings";
import {
    type Theme,
    type TimeControlSettings,
    type VolumeLevel,
    timeControlPresets,
} from "@/types/settings";
import { useState } from "react";
import {
    AlertDialog,
    AlertDialogAction,
    AlertDialogCancel,
    AlertDialogContent,
    AlertDialogDescription,
    AlertDialogFooter,
    AlertDialogHeader,
    AlertDialogTitle,
} from "./ui/alert-dialog";
import { Button } from "./ui/button";

interface SettingsDialogProps {
    isOpen: boolean;
    onOpenChange: (open: boolean) => void;
}

export function SettingsDialog({ isOpen, onOpenChange }: SettingsDialogProps) {
    const { settings, updateTimeControl, updateAudio, updateDisplay, resetSettings } =
        useGameSettings();

    const [showResetConfirm, setShowResetConfirm] = useState(false);

    const handleTimeControlPreset = (preset: TimeControlSettings) => {
        updateTimeControl(preset);
    };

    const handleVolumeChange = (volume: VolumeLevel) => {
        updateAudio({ masterVolume: volume });
    };

    const handleThemeChange = (theme: Theme) => {
        updateDisplay({ theme });
    };

    const handleResetSettings = () => {
        resetSettings();
        setShowResetConfirm(false);
        onOpenChange(false);
    };

    return (
        <AlertDialog open={isOpen} onOpenChange={onOpenChange}>
            <AlertDialogContent className="max-w-2xl max-h-[80vh] overflow-y-auto">
                <AlertDialogHeader>
                    <AlertDialogTitle className="flex items-center gap-2">
                        ⚙️ ゲーム設定
                    </AlertDialogTitle>
                    <AlertDialogDescription>
                        対局時計、音声、表示などの設定を変更できます。
                    </AlertDialogDescription>
                </AlertDialogHeader>

                <div className="space-y-6">
                    {/* 時間制御設定 */}
                    <div className="space-y-4">
                        <h3 className="text-lg font-semibold">⏱️ 時間制御</h3>

                        <div className="flex items-center gap-3">
                            <input
                                type="checkbox"
                                id="time-control-enabled"
                                checked={settings.timeControl.enabled}
                                onChange={(e) => updateTimeControl({ enabled: e.target.checked })}
                                className="w-4 h-4"
                            />
                            <label htmlFor="time-control-enabled" className="text-sm font-medium">
                                時間制限を有効にする
                            </label>
                        </div>

                        {settings.timeControl.enabled && (
                            <div className="space-y-3 pl-7">
                                <div className="grid grid-cols-2 gap-3">
                                    {timeControlPresets.map((preset) => (
                                        <Button
                                            key={preset.name}
                                            type="button"
                                            variant={
                                                settings.timeControl.mainTimeMinutes ===
                                                    preset.settings.mainTimeMinutes &&
                                                settings.timeControl.byoyomiSeconds ===
                                                    preset.settings.byoyomiSeconds
                                                    ? "default"
                                                    : "outline"
                                            }
                                            size="sm"
                                            onClick={() => handleTimeControlPreset(preset.settings)}
                                            className="flex-col h-16"
                                        >
                                            <div className="font-medium">{preset.name}</div>
                                            <div className="text-xs opacity-70">
                                                {preset.description}
                                            </div>
                                        </Button>
                                    ))}
                                </div>

                                <div className="grid grid-cols-2 gap-4 p-3 bg-gray-50 rounded-lg">
                                    <div>
                                        <label
                                            htmlFor="main-time-slider"
                                            className="block text-sm font-medium mb-2"
                                        >
                                            持ち時間: {settings.timeControl.mainTimeMinutes}分
                                        </label>
                                        <input
                                            id="main-time-slider"
                                            type="range"
                                            min="1"
                                            max="90"
                                            step="1"
                                            value={settings.timeControl.mainTimeMinutes}
                                            onChange={(e) =>
                                                updateTimeControl({
                                                    mainTimeMinutes: Number(
                                                        e.target.value,
                                                    ) as TimeControlSettings["mainTimeMinutes"],
                                                })
                                            }
                                            className="w-full"
                                        />
                                    </div>
                                    <div>
                                        <label
                                            htmlFor="byoyomi-slider"
                                            className="block text-sm font-medium mb-2"
                                        >
                                            秒読み: {settings.timeControl.byoyomiSeconds}秒
                                        </label>
                                        <input
                                            id="byoyomi-slider"
                                            type="range"
                                            min="0"
                                            max="60"
                                            step="10"
                                            value={settings.timeControl.byoyomiSeconds}
                                            onChange={(e) =>
                                                updateTimeControl({
                                                    byoyomiSeconds: Number(
                                                        e.target.value,
                                                    ) as TimeControlSettings["byoyomiSeconds"],
                                                })
                                            }
                                            className="w-full"
                                        />
                                    </div>
                                </div>
                            </div>
                        )}
                    </div>

                    {/* 音声設定 */}
                    <div className="space-y-4">
                        <h3 className="text-lg font-semibold">🔊 音声設定</h3>

                        <div className="space-y-3">
                            <div>
                                <label
                                    htmlFor="volume-slider"
                                    className="block text-sm font-medium mb-2"
                                >
                                    音量: {settings.audio.masterVolume}%
                                </label>
                                <input
                                    id="volume-slider"
                                    type="range"
                                    min="0"
                                    max="100"
                                    step="25"
                                    value={settings.audio.masterVolume}
                                    onChange={(e) =>
                                        handleVolumeChange(Number(e.target.value) as VolumeLevel)
                                    }
                                    className="w-full"
                                />
                                <div className="flex justify-between text-xs text-gray-500 mt-1">
                                    <span>無音</span>
                                    <span>小</span>
                                    <span>中</span>
                                    <span>大</span>
                                    <span>最大</span>
                                </div>
                            </div>

                            <div className="grid grid-cols-1 gap-2">
                                <label className="flex items-center gap-2">
                                    <input
                                        type="checkbox"
                                        checked={settings.audio.pieceSound}
                                        onChange={(e) =>
                                            updateAudio({ pieceSound: e.target.checked })
                                        }
                                        className="w-4 h-4"
                                    />
                                    <span className="text-sm">駒音</span>
                                </label>
                                <label className="flex items-center gap-2">
                                    <input
                                        type="checkbox"
                                        checked={settings.audio.checkSound}
                                        onChange={(e) =>
                                            updateAudio({ checkSound: e.target.checked })
                                        }
                                        className="w-4 h-4"
                                    />
                                    <span className="text-sm">王手音</span>
                                </label>
                                <label className="flex items-center gap-2">
                                    <input
                                        type="checkbox"
                                        checked={settings.audio.gameEndSound}
                                        onChange={(e) =>
                                            updateAudio({ gameEndSound: e.target.checked })
                                        }
                                        className="w-4 h-4"
                                    />
                                    <span className="text-sm">ゲーム終了音</span>
                                </label>
                            </div>
                        </div>
                    </div>

                    {/* 表示設定 */}
                    <div className="space-y-4">
                        <h3 className="text-lg font-semibold">🎨 表示設定</h3>

                        <div className="space-y-3">
                            <div>
                                <div className="block text-sm font-medium mb-2">テーマ</div>
                                <div className="flex gap-2">
                                    {(["light", "dark", "auto"] as Theme[]).map((theme) => (
                                        <Button
                                            key={theme}
                                            type="button"
                                            variant={
                                                settings.display.theme === theme
                                                    ? "default"
                                                    : "outline"
                                            }
                                            size="sm"
                                            onClick={() => handleThemeChange(theme)}
                                        >
                                            {theme === "light"
                                                ? "ライト"
                                                : theme === "dark"
                                                  ? "ダーク"
                                                  : "自動"}
                                        </Button>
                                    ))}
                                </div>
                            </div>

                            <div className="grid grid-cols-1 gap-2">
                                <label className="flex items-center gap-2">
                                    <input
                                        type="checkbox"
                                        checked={settings.display.animations}
                                        onChange={(e) =>
                                            updateDisplay({ animations: e.target.checked })
                                        }
                                        className="w-4 h-4"
                                    />
                                    <span className="text-sm">アニメーション</span>
                                </label>
                                <label className="flex items-center gap-2">
                                    <input
                                        type="checkbox"
                                        checked={settings.display.showValidMoves}
                                        onChange={(e) =>
                                            updateDisplay({ showValidMoves: e.target.checked })
                                        }
                                        className="w-4 h-4"
                                    />
                                    <span className="text-sm">有効手のハイライト</span>
                                </label>
                                <label className="flex items-center gap-2">
                                    <input
                                        type="checkbox"
                                        checked={settings.display.showLastMove}
                                        onChange={(e) =>
                                            updateDisplay({ showLastMove: e.target.checked })
                                        }
                                        className="w-4 h-4"
                                    />
                                    <span className="text-sm">最後の手をハイライト</span>
                                </label>
                            </div>
                        </div>
                    </div>
                </div>

                <AlertDialogFooter className="flex gap-2">
                    <Button
                        type="button"
                        variant="outline"
                        onClick={() => setShowResetConfirm(true)}
                    >
                        設定をリセット
                    </Button>
                    <AlertDialogCancel>閉じる</AlertDialogCancel>
                </AlertDialogFooter>

                {/* リセット確認ダイアログ */}
                <AlertDialog open={showResetConfirm} onOpenChange={setShowResetConfirm}>
                    <AlertDialogContent>
                        <AlertDialogHeader>
                            <AlertDialogTitle>設定をリセットしますか？</AlertDialogTitle>
                            <AlertDialogDescription>
                                すべての設定がデフォルト値に戻されます。この操作は元に戻せません。
                            </AlertDialogDescription>
                        </AlertDialogHeader>
                        <AlertDialogFooter>
                            <AlertDialogCancel>キャンセル</AlertDialogCancel>
                            <AlertDialogAction onClick={handleResetSettings}>
                                リセットする
                            </AlertDialogAction>
                        </AlertDialogFooter>
                    </AlertDialogContent>
                </AlertDialog>
            </AlertDialogContent>
        </AlertDialog>
    );
}
