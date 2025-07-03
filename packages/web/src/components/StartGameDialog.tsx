import type { AIDifficulty } from "@/types/ai";
import { useState } from "react";
import {
    AlertDialog,
    AlertDialogContent,
    AlertDialogDescription,
    AlertDialogFooter,
    AlertDialogHeader,
    AlertDialogTitle,
} from "./ui/alert-dialog";
import { Button } from "./ui/button";

interface StartGameDialogProps {
    isOpen: boolean;
    onClose: () => void;
    onStartLocal: () => void;
    onStartAI: (difficulty: AIDifficulty) => Promise<void>;
}

export function StartGameDialog({
    isOpen,
    onClose,
    onStartLocal,
    onStartAI,
}: StartGameDialogProps) {
    const [selectedDifficulty, setSelectedDifficulty] = useState<AIDifficulty>("intermediate");

    const difficulties: { value: AIDifficulty; label: string; description: string }[] = [
        { value: "beginner", label: "初級", description: "簡単な手を指すことがあります" },
        { value: "intermediate", label: "中級", description: "定跡を使い、適度な強さです" },
        { value: "advanced", label: "上級", description: "高度な評価と終盤力があります" },
        { value: "expert", label: "エキスパート", description: "最高難易度、長時間思考します" },
    ];

    const handleStartAI = async () => {
        await onStartAI(selectedDifficulty);
        onClose();
    };

    const handleStartLocal = () => {
        onStartLocal();
        onClose();
    };

    return (
        <AlertDialog open={isOpen} onOpenChange={onClose}>
            <AlertDialogContent className="max-w-md">
                <AlertDialogHeader>
                    <AlertDialogTitle>対局モードを選択</AlertDialogTitle>
                    <AlertDialogDescription>
                        この局面から対局を開始する方式を選択してください。
                    </AlertDialogDescription>
                </AlertDialogHeader>

                <div className="space-y-4">
                    {/* ローカル対戦オプション */}
                    <div className="border rounded-lg p-4">
                        <h3 className="font-medium mb-2">👥 人対人（ローカル対戦）</h3>
                        <p className="text-sm text-gray-600 mb-3">
                            同じデバイスで2人が交代で操作します
                        </p>
                        <Button onClick={handleStartLocal} className="w-full">
                            人対人で開始
                        </Button>
                    </div>

                    {/* AI対戦オプション */}
                    <div className="border rounded-lg p-4">
                        <h3 className="font-medium mb-2">🤖 AI対戦</h3>
                        <p className="text-sm text-gray-600 mb-3">コンピューターと対局します</p>

                        <div className="space-y-2 mb-3">
                            {difficulties.map((diff) => (
                                <label
                                    key={diff.value}
                                    className="flex items-center space-x-2 cursor-pointer"
                                >
                                    <input
                                        type="radio"
                                        name="difficulty"
                                        value={diff.value}
                                        checked={selectedDifficulty === diff.value}
                                        onChange={(e) =>
                                            setSelectedDifficulty(e.target.value as AIDifficulty)
                                        }
                                        className="radio"
                                    />
                                    <div className="flex-1">
                                        <div className="font-medium">{diff.label}</div>
                                        <div className="text-xs text-gray-500">
                                            {diff.description}
                                        </div>
                                    </div>
                                </label>
                            ))}
                        </div>

                        <Button onClick={handleStartAI} className="w-full">
                            AI対戦で開始
                        </Button>
                    </div>
                </div>

                <AlertDialogFooter>
                    <Button variant="outline" onClick={onClose}>
                        キャンセル
                    </Button>
                </AlertDialogFooter>
            </AlertDialogContent>
        </AlertDialog>
    );
}
