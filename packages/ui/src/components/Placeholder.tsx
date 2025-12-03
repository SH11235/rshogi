import type { ReactElement } from "react";
import { cn } from "@shogi/design-system";

export type PlaceholderProps = {
    label?: string;
    className?: string;
};

export function Placeholder({ label = "Shogi UI", className }: PlaceholderProps): ReactElement {
    return (
        <div
            aria-label="shogi-ui-placeholder"
            className={cn(
                "flex flex-col gap-3 rounded-lg border border-dashed border-muted-foreground/50 bg-muted/40 p-4 text-sm text-muted-foreground",
                className,
            )}
        >
            <span className="text-xs font-semibold uppercase tracking-[0.25em] text-primary">
                Design token
            </span>
            <p className="text-base font-medium text-foreground">{label}</p>
            <p>
                Colors reference CSS variables from <code>@shogi/design-system</code>, ensuring
                consistent styling between apps.
            </p>
        </div>
    );
}
