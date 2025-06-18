import { useState } from "react";
import {
    AlertDialog,
    AlertDialogAction,
    AlertDialogContent,
    AlertDialogDescription,
    AlertDialogFooter,
    AlertDialogHeader,
    AlertDialogTitle,
    AlertDialogTrigger,
} from "./ui/alert-dialog";
import { Button } from "./ui/button";

export function KeyboardHelp() {
    const [isOpen, setIsOpen] = useState(false);

    const shortcuts = [
        { key: "Ctrl + Z", description: "元に戻す" },
        { key: "Ctrl + Y", description: "やり直し" },
        { key: "Ctrl + Shift + Z", description: "やり直し（別のキー）" },
        { key: "R", description: "ゲームリセット" },
        { key: "Escape", description: "選択解除・ダイアログクローズ" },
        { key: "Enter", description: "プロモーション時に成る" },
    ];

    return (
        <AlertDialog open={isOpen} onOpenChange={setIsOpen}>
            <AlertDialogTrigger asChild>
                <Button
                    type="button"
                    variant="outline"
                    size="sm"
                    className="text-xs sm:text-sm"
                    title="キーボードショートカット"
                >
                    ⌨️ ヘルプ
                </Button>
            </AlertDialogTrigger>
            <AlertDialogContent className="max-w-md">
                <AlertDialogHeader>
                    <AlertDialogTitle>キーボードショートカット</AlertDialogTitle>
                    <AlertDialogDescription>
                        以下のキーボードショートカットが利用できます：
                    </AlertDialogDescription>
                </AlertDialogHeader>
                <div className="space-y-3">
                    {shortcuts.map((shortcut) => (
                        <div key={shortcut.key} className="flex justify-between items-center">
                            <span className="font-mono text-sm bg-gray-100 px-2 py-1 rounded">
                                {shortcut.key}
                            </span>
                            <span className="text-sm text-gray-700">{shortcut.description}</span>
                        </div>
                    ))}
                </div>
                <AlertDialogFooter>
                    <AlertDialogAction onClick={() => setIsOpen(false)}>閉じる</AlertDialogAction>
                </AlertDialogFooter>
            </AlertDialogContent>
        </AlertDialog>
    );
}
