import { cn } from "@shogi/design-system";
import type { InputHTMLAttributes, ReactElement } from "react";
import { forwardRef } from "react";

export interface InputProps extends InputHTMLAttributes<HTMLInputElement> {}

export const Input = forwardRef<HTMLInputElement, InputProps>(function Input(
    { className, type = "text", ...props },
    ref,
): ReactElement {
    return (
        <input
            className={cn(
                "flex h-10 w-full rounded-md border border-input bg-transparent px-3 py-2 text-sm ring-offset-background placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 disabled:cursor-not-allowed disabled:opacity-50",
                className,
            )}
            ref={ref}
            type={type}
            {...props}
        />
    );
});
