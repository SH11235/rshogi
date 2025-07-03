import { useGameStore } from "@/stores/gameStore";
import {
    TIMER_PRESETS,
    type TimerConfig,
    type TimerMode,
    type TimerPresetKey,
} from "@/types/timer";
import { useState } from "react";
import { Button } from "./ui/button";
import {
    Dialog,
    DialogContent,
    DialogDescription,
    DialogFooter,
    DialogHeader,
    DialogTitle,
} from "./ui/dialog";
import { Label } from "./ui/label";
import { RadioGroup, RadioGroupItem } from "./ui/radio-group";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "./ui/select";

interface TimerSettingsDialogProps {
    open: boolean;
    onOpenChange: (open: boolean) => void;
    isGameInProgress: boolean;
}

const BASIC_TIME_OPTIONS = [
    { value: 300, label: "5分" },
    { value: 600, label: "10分" },
    { value: 1800, label: "30分" },
    { value: 3600, label: "1時間" },
    { value: 7200, label: "2時間" },
];

const BYOYOMI_TIME_OPTIONS = [
    { value: 10, label: "10秒" },
    { value: 30, label: "30秒" },
    { value: 60, label: "1分" },
];

const FISCHER_INCREMENT_OPTIONS = [
    { value: 10, label: "10秒" },
    { value: 30, label: "30秒" },
    { value: 60, label: "1分" },
];

const PER_MOVE_OPTIONS = [
    { value: 30, label: "30秒" },
    { value: 60, label: "1分" },
    { value: 180, label: "3分" },
    { value: 300, label: "5分" },
];

export function TimerSettingsDialog({
    open,
    onOpenChange,
    isGameInProgress,
}: TimerSettingsDialogProps) {
    const initializeTimer = useGameStore((state) => state.initializeTimer);
    const startPlayerTimer = useGameStore((state) => state.startPlayerTimer);
    const isOnlineGame = useGameStore((state) => state.isOnlineGame);
    const connectionStatus = useGameStore((state) => state.connectionStatus);

    const [mode, setMode] = useState<TimerMode>("basic");
    const [basicTime, setBasicTime] = useState(1800); // 30分
    const [byoyomiTime, setByoyomiTime] = useState(60); // 1分
    const [fischerIncrement, setFischerIncrement] = useState(30); // 30秒
    const [perMoveLimit, setPerMoveLimit] = useState(60); // 1分
    const [selectedPreset, setSelectedPreset] = useState<TimerPresetKey | null>(null);

    const handlePresetSelect = (presetKey: TimerPresetKey) => {
        const preset = TIMER_PRESETS[presetKey];
        setSelectedPreset(presetKey);
        if (preset.mode) {
            setMode(preset.mode);
        }
        setBasicTime(preset.basicTime);
        setByoyomiTime(preset.byoyomiTime);
        setFischerIncrement(preset.fischerIncrement);
        setPerMoveLimit(preset.perMoveLimit);
    };

    const handleModeChange = (newMode: TimerMode) => {
        setMode(newMode);
        setSelectedPreset(null); // カスタム設定になるのでプリセット選択を解除
    };

    const handleStart = () => {
        const config: TimerConfig = {
            mode,
            basicTime,
            byoyomiTime,
            fischerIncrement,
            perMoveLimit,
        };

        initializeTimer(config);
        // ゲーム開始時は先手のタイマーをスタート
        if (!isGameInProgress) {
            startPlayerTimer("black");
        }
        onOpenChange(false);
    };

    return (
        <Dialog open={open} onOpenChange={onOpenChange}>
            <DialogContent className="sm:max-w-[500px]">
                <DialogHeader>
                    <DialogTitle>対局時計の設定</DialogTitle>
                    <DialogDescription>
                        持ち時間の方式と時間を設定してください
                        {isOnlineGame && !connectionStatus.isHost && (
                            <span className="block text-yellow-600 mt-2">
                                ※ 通信対戦ではホストのみがタイマー設定を変更できます
                            </span>
                        )}
                    </DialogDescription>
                </DialogHeader>

                <div className="space-y-4">
                    {/* プリセット選択 */}
                    <div>
                        <Label className="text-sm font-medium mb-2 block">クイック設定</Label>
                        <div className="grid grid-cols-2 gap-2">
                            <Button
                                type="button"
                                variant={selectedPreset === "rapid10" ? "default" : "outline"}
                                size="sm"
                                onClick={() => handlePresetSelect("rapid10")}
                            >
                                早指し10分
                            </Button>
                            <Button
                                type="button"
                                variant={selectedPreset === "normal30" ? "default" : "outline"}
                                size="sm"
                                onClick={() => handlePresetSelect("normal30")}
                            >
                                通常30分
                            </Button>
                            <Button
                                type="button"
                                variant={
                                    selectedPreset === "fischer10plus30" ? "default" : "outline"
                                }
                                size="sm"
                                onClick={() => handlePresetSelect("fischer10plus30")}
                            >
                                フィッシャー10分+30秒
                            </Button>
                            <Button
                                type="button"
                                variant={selectedPreset === "perMove1min" ? "default" : "outline"}
                                size="sm"
                                onClick={() => handlePresetSelect("perMove1min")}
                            >
                                一手1分
                            </Button>
                        </div>
                    </div>

                    <div className="border-t pt-4">
                        <Label className="text-sm font-medium mb-2 block">詳細設定</Label>

                        {/* 時間方式選択 */}
                        <RadioGroup
                            value={mode}
                            onValueChange={(value) => handleModeChange(value as TimerMode)}
                        >
                            <div className="space-y-2">
                                <div className="flex items-center space-x-2">
                                    <RadioGroupItem value="basic" id="basic" />
                                    <Label htmlFor="basic" className="font-normal cursor-pointer">
                                        基本時間＋秒読み
                                    </Label>
                                </div>
                                <div className="flex items-center space-x-2">
                                    <RadioGroupItem value="fischer" id="fischer" />
                                    <Label htmlFor="fischer" className="font-normal cursor-pointer">
                                        フィッシャー方式
                                    </Label>
                                </div>
                                <div className="flex items-center space-x-2">
                                    <RadioGroupItem value="perMove" id="perMove" />
                                    <Label htmlFor="perMove" className="font-normal cursor-pointer">
                                        一手制限
                                    </Label>
                                </div>
                            </div>
                        </RadioGroup>

                        {/* 時間設定 */}
                        <div className="mt-4 space-y-3">
                            {mode === "basic" && (
                                <>
                                    <div>
                                        <Label htmlFor="basicTime" className="text-sm">
                                            基本時間
                                        </Label>
                                        <Select
                                            value={basicTime.toString()}
                                            onValueChange={(value: string) => {
                                                setBasicTime(Number.parseInt(value));
                                                setSelectedPreset(null);
                                            }}
                                        >
                                            <SelectTrigger id="basicTime">
                                                <SelectValue />
                                            </SelectTrigger>
                                            <SelectContent>
                                                {BASIC_TIME_OPTIONS.map((option) => (
                                                    <SelectItem
                                                        key={option.value}
                                                        value={option.value.toString()}
                                                    >
                                                        {option.label}
                                                    </SelectItem>
                                                ))}
                                            </SelectContent>
                                        </Select>
                                    </div>
                                    <div>
                                        <Label htmlFor="byoyomiTime" className="text-sm">
                                            秒読み
                                        </Label>
                                        <Select
                                            value={byoyomiTime.toString()}
                                            onValueChange={(value: string) => {
                                                setByoyomiTime(Number.parseInt(value));
                                                setSelectedPreset(null);
                                            }}
                                        >
                                            <SelectTrigger id="byoyomiTime">
                                                <SelectValue />
                                            </SelectTrigger>
                                            <SelectContent>
                                                {BYOYOMI_TIME_OPTIONS.map((option) => (
                                                    <SelectItem
                                                        key={option.value}
                                                        value={option.value.toString()}
                                                    >
                                                        {option.label}
                                                    </SelectItem>
                                                ))}
                                            </SelectContent>
                                        </Select>
                                    </div>
                                </>
                            )}

                            {mode === "fischer" && (
                                <>
                                    <div>
                                        <Label htmlFor="fischerBasicTime" className="text-sm">
                                            初期時間
                                        </Label>
                                        <Select
                                            value={basicTime.toString()}
                                            onValueChange={(value: string) => {
                                                setBasicTime(Number.parseInt(value));
                                                setSelectedPreset(null);
                                            }}
                                        >
                                            <SelectTrigger id="fischerBasicTime">
                                                <SelectValue />
                                            </SelectTrigger>
                                            <SelectContent>
                                                {BASIC_TIME_OPTIONS.map((option) => (
                                                    <SelectItem
                                                        key={option.value}
                                                        value={option.value.toString()}
                                                    >
                                                        {option.label}
                                                    </SelectItem>
                                                ))}
                                            </SelectContent>
                                        </Select>
                                    </div>
                                    <div>
                                        <Label htmlFor="fischerIncrement" className="text-sm">
                                            加算時間
                                        </Label>
                                        <Select
                                            value={fischerIncrement.toString()}
                                            onValueChange={(value: string) => {
                                                setFischerIncrement(Number.parseInt(value));
                                                setSelectedPreset(null);
                                            }}
                                        >
                                            <SelectTrigger id="fischerIncrement">
                                                <SelectValue />
                                            </SelectTrigger>
                                            <SelectContent>
                                                {FISCHER_INCREMENT_OPTIONS.map((option) => (
                                                    <SelectItem
                                                        key={option.value}
                                                        value={option.value.toString()}
                                                    >
                                                        {option.label}
                                                    </SelectItem>
                                                ))}
                                            </SelectContent>
                                        </Select>
                                    </div>
                                </>
                            )}

                            {mode === "perMove" && (
                                <div>
                                    <Label htmlFor="perMoveLimit" className="text-sm">
                                        制限時間
                                    </Label>
                                    <Select
                                        value={perMoveLimit.toString()}
                                        onValueChange={(value) => {
                                            setPerMoveLimit(Number.parseInt(value));
                                            setSelectedPreset(null);
                                        }}
                                    >
                                        <SelectTrigger id="perMoveLimit">
                                            <SelectValue />
                                        </SelectTrigger>
                                        <SelectContent>
                                            {PER_MOVE_OPTIONS.map((option) => (
                                                <SelectItem
                                                    key={option.value}
                                                    value={option.value.toString()}
                                                >
                                                    {option.label}
                                                </SelectItem>
                                            ))}
                                        </SelectContent>
                                    </Select>
                                </div>
                            )}
                        </div>
                    </div>
                </div>

                <DialogFooter>
                    <Button type="button" variant="outline" onClick={() => onOpenChange(false)}>
                        キャンセル
                    </Button>
                    <Button type="button" onClick={handleStart}>
                        開始
                    </Button>
                </DialogFooter>
            </DialogContent>
        </Dialog>
    );
}
