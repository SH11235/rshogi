import { useState } from "react";
import { SettingsDialog } from "./SettingsDialog";
import { Button } from "./ui/button";

export function SettingsButton() {
    const [isOpen, setIsOpen] = useState(false);

    return (
        <>
            <Button
                type="button"
                variant="outline"
                size="sm"
                onClick={() => setIsOpen(true)}
                className="text-xs sm:text-sm"
                title="ゲーム設定"
            >
                ⚙️ 設定
            </Button>
            <SettingsDialog isOpen={isOpen} onOpenChange={setIsOpen} />
        </>
    );
}
