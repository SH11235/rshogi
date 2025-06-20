import { ChevronFirst, ChevronLast, ChevronLeft, ChevronRight, Undo2 } from "lucide-react";
import { Button } from "./ui/button";
import { Card, CardContent } from "./ui/card";

interface PlaybackControlsProps {
    canNavigateNext: boolean;
    canNavigatePrevious: boolean;
    isInBranch: boolean;
    onNavigateFirst: () => void;
    onNavigatePrevious: () => void;
    onNavigateNext: () => void;
    onNavigateLast: () => void;
    onReturnToMainLine: () => void;
}

export function PlaybackControls({
    canNavigateNext,
    canNavigatePrevious,
    isInBranch,
    onNavigateFirst,
    onNavigatePrevious,
    onNavigateNext,
    onNavigateLast,
    onReturnToMainLine,
}: PlaybackControlsProps) {
    return (
        <Card className="w-80 sm:w-96">
            <CardContent className="p-2 sm:p-3">
                <div className="flex items-center gap-1">
                    <Button
                        onClick={onNavigateFirst}
                        disabled={!canNavigatePrevious}
                        size="sm"
                        variant="outline"
                        className="touch-manipulation"
                        title="最初へ (Home)"
                    >
                        <ChevronFirst className="h-4 w-4" />
                    </Button>
                    <Button
                        onClick={onNavigatePrevious}
                        disabled={!canNavigatePrevious}
                        size="sm"
                        variant="outline"
                        className="flex-1 touch-manipulation"
                        title="前へ (←)"
                    >
                        <ChevronLeft className="h-4 w-4 mr-1" />
                        前へ
                    </Button>
                    <Button
                        onClick={onNavigateNext}
                        disabled={!canNavigateNext}
                        size="sm"
                        variant="outline"
                        className="flex-1 touch-manipulation"
                        title="次へ (→)"
                    >
                        次へ
                        <ChevronRight className="h-4 w-4 ml-1" />
                    </Button>
                    <Button
                        onClick={onNavigateLast}
                        disabled={!canNavigateNext}
                        size="sm"
                        variant="outline"
                        className="touch-manipulation"
                        title="最後へ (End)"
                    >
                        <ChevronLast className="h-4 w-4" />
                    </Button>
                    {isInBranch && (
                        <Button
                            onClick={onReturnToMainLine}
                            size="sm"
                            variant="secondary"
                            className="touch-manipulation"
                            title="本譜に戻る (M)"
                        >
                            <Undo2 className="h-4 w-4" />
                        </Button>
                    )}
                </div>
            </CardContent>
        </Card>
    );
}
