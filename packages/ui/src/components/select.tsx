/**
 * Select コンポーネント
 *
 * Radix UI Select をベースにしたセレクトボックス
 */

import * as SelectPrimitive from "@radix-ui/react-select";
import { cn } from "@shogi/design-system";
import type { ComponentPropsWithoutRef, ComponentRef, ReactElement } from "react";
import { forwardRef } from "react";

const Select = SelectPrimitive.Root;
const SelectValue = SelectPrimitive.Value;

const SelectTrigger = forwardRef<
    ComponentRef<typeof SelectPrimitive.Trigger>,
    ComponentPropsWithoutRef<typeof SelectPrimitive.Trigger>
>(function SelectTrigger({ className, children, ...props }, ref): ReactElement {
    return (
        <SelectPrimitive.Trigger
            className={cn(
                "flex w-full items-center justify-between rounded-lg border border-wafuu-border bg-wafuu-washi px-3 py-2 text-xs",
                "placeholder:text-muted-foreground",
                "focus:outline-none focus:ring-2 focus:ring-ring focus:ring-offset-2",
                "disabled:cursor-not-allowed disabled:opacity-50",
                "[&>span]:line-clamp-1",
                className,
            )}
            ref={ref}
            {...props}
        >
            {children}
            <SelectPrimitive.Icon asChild>
                <svg
                    className="h-4 w-4 opacity-50 shrink-0 ml-2"
                    xmlns="http://www.w3.org/2000/svg"
                    viewBox="0 0 20 20"
                    fill="currentColor"
                    aria-hidden="true"
                >
                    <path
                        fillRule="evenodd"
                        d="M5.23 7.21a.75.75 0 011.06.02L10 11.168l3.71-3.938a.75.75 0 111.08 1.04l-4.25 4.5a.75.75 0 01-1.08 0l-4.25-4.5a.75.75 0 01.02-1.06z"
                        clipRule="evenodd"
                    />
                </svg>
            </SelectPrimitive.Icon>
        </SelectPrimitive.Trigger>
    );
});

const SelectContent = forwardRef<
    ComponentRef<typeof SelectPrimitive.Content>,
    ComponentPropsWithoutRef<typeof SelectPrimitive.Content>
>(function SelectContent(
    { className, children, position = "popper", ...props },
    ref,
): ReactElement {
    return (
        <SelectPrimitive.Portal>
            <SelectPrimitive.Content
                className={cn(
                    "relative z-50 max-h-96 min-w-[8rem] overflow-hidden rounded-lg border border-wafuu-border bg-wafuu-washi shadow-md",
                    "data-[state=open]:animate-in data-[state=closed]:animate-out",
                    "data-[state=closed]:fade-out-0 data-[state=open]:fade-in-0",
                    "data-[state=closed]:zoom-out-95 data-[state=open]:zoom-in-95",
                    "data-[side=bottom]:slide-in-from-top-2 data-[side=left]:slide-in-from-right-2",
                    "data-[side=right]:slide-in-from-left-2 data-[side=top]:slide-in-from-bottom-2",
                    position === "popper" &&
                        "data-[side=bottom]:translate-y-1 data-[side=left]:-translate-x-1 data-[side=right]:translate-x-1 data-[side=top]:-translate-y-1",
                    className,
                )}
                position={position}
                ref={ref}
                {...props}
            >
                <SelectPrimitive.Viewport
                    className={cn(
                        "p-1",
                        position === "popper" &&
                            "h-[var(--radix-select-trigger-height)] w-full min-w-[var(--radix-select-trigger-width)]",
                    )}
                >
                    {children}
                </SelectPrimitive.Viewport>
            </SelectPrimitive.Content>
        </SelectPrimitive.Portal>
    );
});

const SelectItem = forwardRef<
    ComponentRef<typeof SelectPrimitive.Item>,
    ComponentPropsWithoutRef<typeof SelectPrimitive.Item>
>(function SelectItem({ className, children, ...props }, ref): ReactElement {
    return (
        <SelectPrimitive.Item
            className={cn(
                "relative flex w-full cursor-default select-none items-center rounded-md py-1.5 pl-8 pr-2 text-xs outline-none",
                "focus:bg-wafuu-border focus:text-wafuu-sumi",
                "data-[disabled]:pointer-events-none data-[disabled]:opacity-50",
                className,
            )}
            ref={ref}
            {...props}
        >
            <span className="absolute left-2 flex h-3.5 w-3.5 items-center justify-center">
                <SelectPrimitive.ItemIndicator>
                    <svg
                        className="h-4 w-4"
                        xmlns="http://www.w3.org/2000/svg"
                        viewBox="0 0 20 20"
                        fill="currentColor"
                        aria-hidden="true"
                    >
                        <path
                            fillRule="evenodd"
                            d="M16.704 4.153a.75.75 0 01.143 1.052l-8 10.5a.75.75 0 01-1.127.075l-4.5-4.5a.75.75 0 011.06-1.06l3.894 3.893 7.48-9.817a.75.75 0 011.05-.143z"
                            clipRule="evenodd"
                        />
                    </svg>
                </SelectPrimitive.ItemIndicator>
            </span>
            <SelectPrimitive.ItemText>{children}</SelectPrimitive.ItemText>
        </SelectPrimitive.Item>
    );
});

export { Select, SelectContent, SelectItem, SelectTrigger, SelectValue };
