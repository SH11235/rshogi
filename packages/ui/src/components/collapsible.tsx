import * as CollapsiblePrimitive from "@radix-ui/react-collapsible";
import { cn } from "@shogi/design-system";
import type { ComponentPropsWithoutRef, ElementRef, ReactElement } from "react";
import { forwardRef } from "react";

export const Collapsible = CollapsiblePrimitive.Root;
export const CollapsibleTrigger = CollapsiblePrimitive.Trigger;

export const CollapsibleContent = forwardRef<
    ElementRef<typeof CollapsiblePrimitive.Content>,
    ComponentPropsWithoutRef<typeof CollapsiblePrimitive.Content>
>(function CollapsibleContent({ className, children, ...props }, ref): ReactElement {
    return (
        <CollapsiblePrimitive.Content
            className={cn(
                "overflow-hidden transition-all data-[state=closed]:animate-accordion-up data-[state=open]:animate-accordion-down",
                className,
            )}
            ref={ref}
            {...props}
        >
            {children}
        </CollapsiblePrimitive.Content>
    );
});
