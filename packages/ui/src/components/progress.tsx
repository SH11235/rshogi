import * as ProgressPrimitive from "@radix-ui/react-progress";
import { cn } from "@shogi/design-system";
import type { ComponentPropsWithoutRef, ComponentRef, ReactElement } from "react";
import { forwardRef } from "react";

interface ProgressProps
    extends Omit<ComponentPropsWithoutRef<typeof ProgressPrimitive.Root>, "value"> {
    /** 進捗値 (0-100)。undefined の場合は不確定（indeterminate）モード */
    value?: number;
    /** インジケーターのカスタムクラス */
    indicatorClassName?: string;
}

export const Progress = forwardRef<ComponentRef<typeof ProgressPrimitive.Root>, ProgressProps>(
    function Progress(
        { className, value, indicatorClassName, style, ...props },
        ref,
    ): ReactElement {
        const isIndeterminate = value === undefined;

        return (
            <ProgressPrimitive.Root
                ref={ref}
                style={{
                    position: "relative",
                    height: "8px",
                    overflow: "hidden",
                    borderRadius: "9999px",
                    backgroundColor: "hsl(var(--muted, 0 0% 90%))",
                    ...style,
                }}
                className={cn("w-full", className)}
                value={isIndeterminate ? undefined : value}
                {...props}
            >
                <ProgressPrimitive.Indicator
                    style={{
                        height: "100%",
                        width: isIndeterminate ? "40%" : `${value ?? 0}%`,
                        backgroundColor: "hsl(var(--primary, 220 90% 56%))",
                        borderRadius: "9999px",
                        transition: isIndeterminate ? "none" : "width 150ms ease-out",
                        ...(isIndeterminate
                            ? {
                                  animation: "progress-indeterminate 1.5s ease-in-out infinite",
                              }
                            : {}),
                    }}
                    className={indicatorClassName}
                />
            </ProgressPrimitive.Root>
        );
    },
);

/**
 * CSS for indeterminate animation (add to global styles or use a style tag):
 *
 * @keyframes progress-indeterminate {
 *   0% { transform: translateX(-100%); }
 *   50% { transform: translateX(150%); }
 *   100% { transform: translateX(-100%); }
 * }
 */
