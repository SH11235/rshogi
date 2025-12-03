import * as TooltipPrimitive from "@radix-ui/react-tooltip";
import { cn } from "@shogi/design-system";
import type { ComponentPropsWithoutRef, ElementRef, ReactElement } from "react";
import { forwardRef } from "react";

export const TooltipProvider = TooltipPrimitive.Provider;
export const Tooltip = TooltipPrimitive.Root;
export const TooltipTrigger = TooltipPrimitive.Trigger;

export const TooltipContent = forwardRef<
    ElementRef<typeof TooltipPrimitive.Content>,
    ComponentPropsWithoutRef<typeof TooltipPrimitive.Content>
>(function TooltipContent({ className, sideOffset = 6, ...props }, ref): ReactElement {
    return (
        <TooltipPrimitive.Content
            className={cn(
                "z-50 overflow-hidden rounded-md border bg-popover px-3 py-1.5 text-xs text-popover-foreground shadow-md animate-in fade-in-0 zoom-in-95 data-[state=closed]:animate-out data-[state=closed]:fade-out-0 data-[state=closed]:zoom-out-95 data-[side=bottom]:slide-in-from-top-2 data-[side=top]:slide-in-from-bottom-2 data-[side=left]:slide-in-from-right-2 data-[side=right]:slide-in-from-left-2",
                className,
            )}
            ref={ref}
            sideOffset={sideOffset}
            {...props}
        />
    );
});
