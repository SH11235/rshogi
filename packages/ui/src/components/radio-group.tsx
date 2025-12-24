import * as RadioGroupPrimitive from "@radix-ui/react-radio-group";
import { cn } from "@shogi/design-system";
import type { ComponentPropsWithoutRef, ComponentRef, ReactElement } from "react";
import { forwardRef } from "react";

export const RadioGroup = forwardRef<
    ComponentRef<typeof RadioGroupPrimitive.Root>,
    ComponentPropsWithoutRef<typeof RadioGroupPrimitive.Root>
>(function RadioGroup({ className, ...props }, ref): ReactElement {
    return (
        <RadioGroupPrimitive.Root className={cn("grid gap-2", className)} ref={ref} {...props} />
    );
});

export const RadioGroupItem = forwardRef<
    ComponentRef<typeof RadioGroupPrimitive.Item>,
    ComponentPropsWithoutRef<typeof RadioGroupPrimitive.Item>
>(function RadioGroupItem({ className, ...props }, ref): ReactElement {
    return (
        <RadioGroupPrimitive.Item
            ref={ref}
            className={cn(
                "aspect-square h-4 w-4 rounded-full border border-[hsl(var(--wafuu-border))] text-[hsl(var(--wafuu-shu))] ring-offset-background focus:outline-none focus-visible:ring-2 focus-visible:ring-[hsl(var(--wafuu-shu))] focus-visible:ring-offset-2 disabled:cursor-not-allowed disabled:opacity-50",
                className,
            )}
            {...props}
        >
            <RadioGroupPrimitive.Indicator className="flex items-center justify-center">
                <span className="h-2.5 w-2.5 rounded-full bg-current" />
            </RadioGroupPrimitive.Indicator>
        </RadioGroupPrimitive.Item>
    );
});
