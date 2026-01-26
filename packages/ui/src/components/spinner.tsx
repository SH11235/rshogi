import { cn } from "@shogi/design-system";
import { cva, type VariantProps } from "class-variance-authority";
import type { HTMLAttributes, ReactElement } from "react";

const spinnerVariants = cva("inline-block animate-spin rounded-full border-solid border-current", {
    variants: {
        size: {
            sm: "h-4 w-4 border-2",
            md: "h-6 w-6 border-2",
            lg: "h-8 w-8 border-[3px]",
            xl: "h-12 w-12 border-4",
        },
    },
    defaultVariants: {
        size: "md",
    },
});

interface SpinnerProps
    extends HTMLAttributes<HTMLDivElement>,
        VariantProps<typeof spinnerVariants> {
    /** ローディング中のラベル（アクセシビリティ用） */
    label?: string;
}

export function Spinner({
    className,
    size,
    label = "読み込み中...",
    style,
    ...props
}: SpinnerProps): ReactElement {
    return (
        <div
            aria-live="polite"
            aria-atomic="true"
            aria-busy="true"
            style={{
                borderTopColor: "transparent",
                borderRightColor: "transparent",
                ...style,
            }}
            className={cn(spinnerVariants({ size }), "text-primary", className)}
            {...props}
        >
            <span className="sr-only">{label}</span>
        </div>
    );
}
