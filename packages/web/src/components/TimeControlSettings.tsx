import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
    Select,
    SelectContent,
    SelectItem,
    SelectTrigger,
    SelectValue,
} from "@/components/ui/select";
import { useGameStore } from "@/stores/gameStore";
import type { TimerConfig } from "@/types/timer";
import { useState } from "react";

interface TimeControlSettingsProps {
    onClose?: () => void;
}

export function TimeControlSettings({ onClose }: TimeControlSettingsProps) {
    const { initializeTimer, timer } = useGameStore();

    // Initialize from current settings or defaults
    const [mode, setMode] = useState<TimerConfig["mode"]>(timer.config.mode || "basic");
    const [basicTime, setBasicTime] = useState(timer.config.basicTime || 600); // 10 minutes default
    const [byoyomiTime, setByoyomiTime] = useState(timer.config.byoyomiTime || 30); // 30 seconds default
    const [fischerIncrement, setFischerIncrement] = useState(timer.config.fischerIncrement || 10); // 10 seconds default
    const [perMoveLimit, setPerMoveLimit] = useState(timer.config.perMoveLimit || 60); // 60 seconds default

    const handleApply = () => {
        if (!mode) return;

        const config: TimerConfig = {
            mode,
            basicTime,
            byoyomiTime,
            fischerIncrement,
            perMoveLimit,
        };

        initializeTimer(config);
        onClose?.();
    };

    const handleModeChange = (value: string) => {
        setMode(value as TimerConfig["mode"]);
    };

    const formatTime = (seconds: number): string => {
        const mins = Math.floor(seconds / 60);
        const secs = seconds % 60;
        return `${mins}分${secs > 0 ? `${secs}秒` : ""}`;
    };

    return (
        <div className="space-y-6">
            <Card className="p-4">
                <Label htmlFor="time-mode">時間制度</Label>
                <Select value={mode || ""} onValueChange={handleModeChange}>
                    <SelectTrigger id="time-mode" className="w-full">
                        <SelectValue placeholder="時間制度を選択" />
                    </SelectTrigger>
                    <SelectContent>
                        <SelectItem value="basic">持ち時間 + 秒読み</SelectItem>
                        <SelectItem value="fischer">フィッシャールール</SelectItem>
                        <SelectItem value="perMove">一手制限</SelectItem>
                    </SelectContent>
                </Select>
            </Card>

            {mode === "basic" && (
                <>
                    <Card className="p-4 space-y-4">
                        <div>
                            <Label htmlFor="basic-time">持ち時間 (分)</Label>
                            <div className="flex items-center gap-2">
                                <Input
                                    id="basic-time"
                                    type="number"
                                    min="0"
                                    max="180"
                                    value={Math.floor(basicTime / 60)}
                                    onChange={(e) =>
                                        setBasicTime(Number.parseInt(e.target.value) * 60)
                                    }
                                    className="w-24"
                                />
                                <span className="text-sm text-gray-600">
                                    分 ({formatTime(basicTime)})
                                </span>
                            </div>
                        </div>

                        <div>
                            <Label htmlFor="byoyomi-time">秒読み (秒)</Label>
                            <div className="flex items-center gap-2">
                                <Input
                                    id="byoyomi-time"
                                    type="number"
                                    min="10"
                                    max="60"
                                    step="10"
                                    value={byoyomiTime}
                                    onChange={(e) =>
                                        setByoyomiTime(Number.parseInt(e.target.value))
                                    }
                                    className="w-24"
                                />
                                <span className="text-sm text-gray-600">秒</span>
                            </div>
                        </div>
                    </Card>

                    <div className="text-sm text-gray-600">
                        持ち時間を使い切ると、一手ごとに{byoyomiTime}秒の秒読みになります
                    </div>
                </>
            )}

            {mode === "fischer" && (
                <>
                    <Card className="p-4 space-y-4">
                        <div>
                            <Label htmlFor="fischer-time">持ち時間 (分)</Label>
                            <div className="flex items-center gap-2">
                                <Input
                                    id="fischer-time"
                                    type="number"
                                    min="0"
                                    max="60"
                                    value={Math.floor(basicTime / 60)}
                                    onChange={(e) =>
                                        setBasicTime(Number.parseInt(e.target.value) * 60)
                                    }
                                    className="w-24"
                                />
                                <span className="text-sm text-gray-600">
                                    分 ({formatTime(basicTime)})
                                </span>
                            </div>
                        </div>

                        <div>
                            <Label htmlFor="fischer-increment">加算時間 (秒)</Label>
                            <div className="flex items-center gap-2">
                                <Input
                                    id="fischer-increment"
                                    type="number"
                                    min="0"
                                    max="30"
                                    value={fischerIncrement}
                                    onChange={(e) =>
                                        setFischerIncrement(Number.parseInt(e.target.value))
                                    }
                                    className="w-24"
                                />
                                <span className="text-sm text-gray-600">秒</span>
                            </div>
                        </div>
                    </Card>

                    <div className="text-sm text-gray-600">
                        各手番終了時に{fischerIncrement}秒が加算されます
                    </div>
                </>
            )}

            {mode === "perMove" && (
                <>
                    <Card className="p-4">
                        <Label htmlFor="per-move-limit">一手制限時間 (秒)</Label>
                        <div className="flex items-center gap-2">
                            <Input
                                id="per-move-limit"
                                type="number"
                                min="10"
                                max="300"
                                step="10"
                                value={perMoveLimit}
                                onChange={(e) => setPerMoveLimit(Number.parseInt(e.target.value))}
                                className="w-24"
                            />
                            <span className="text-sm text-gray-600">秒</span>
                        </div>
                    </Card>

                    <div className="text-sm text-gray-600">
                        各手番ごとに{perMoveLimit}秒の制限時間が設定されます
                    </div>
                </>
            )}

            <div className="flex gap-2 justify-end">
                {onClose && (
                    <Button variant="outline" onClick={onClose}>
                        キャンセル
                    </Button>
                )}
                <Button onClick={handleApply} disabled={!mode}>
                    適用
                </Button>
            </div>
        </div>
    );
}
