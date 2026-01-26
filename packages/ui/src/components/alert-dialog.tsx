import * as AlertDialogPrimitive from "@radix-ui/react-alert-dialog";
import { cn } from "@shogi/design-system";
import type { ComponentPropsWithoutRef, ComponentRef, ReactElement } from "react";
import { forwardRef } from "react";
import { buttonVariants } from "./button";

export const AlertDialog = AlertDialogPrimitive.Root;
export const AlertDialogTrigger = AlertDialogPrimitive.Trigger;
const AlertDialogPortal = AlertDialogPrimitive.Portal;

const AlertDialogOverlay = forwardRef<
    ComponentRef<typeof AlertDialogPrimitive.Overlay>,
    ComponentPropsWithoutRef<typeof AlertDialogPrimitive.Overlay>
>(function AlertDialogOverlay({ className, ...props }, ref): ReactElement {
    return (
        <AlertDialogPrimitive.Overlay
            className={cn(
                "fixed inset-0 z-50 bg-black/70 backdrop-blur-sm data-[state=open]:animate-in data-[state=closed]:animate-out data-[state=closed]:fade-out data-[state=open]:fade-in",
                className,
            )}
            ref={ref}
            {...props}
        />
    );
});

export const AlertDialogContent = forwardRef<
    ComponentRef<typeof AlertDialogPrimitive.Content>,
    ComponentPropsWithoutRef<typeof AlertDialogPrimitive.Content>
>(function AlertDialogContent({ className, children, ...props }, ref): ReactElement {
    return (
        <AlertDialogPortal>
            <AlertDialogOverlay />
            <AlertDialogPrimitive.Content
                style={{
                    position: "fixed",
                    top: "50%",
                    left: "50%",
                    transform: "translate(-50%, -50%)",
                    width: "min(420px, calc(100% - 32px))",
                    backgroundColor: "hsl(var(--card, 0 0% 100%))",
                    color: "hsl(var(--foreground, 0 0% 10%))",
                    border: "1px solid hsl(var(--border, 0 0% 86%))",
                    borderRadius: "12px",
                    boxShadow: "0 24px 70px rgba(0, 0, 0, 0.35)",
                    padding: "24px",
                    zIndex: 51,
                }}
                className={cn(
                    "grid gap-4 duration-200 data-[state=open]:animate-in data-[state=closed]:animate-out data-[state=closed]:fade-out data-[state=open]:fade-in data-[state=closed]:zoom-out-95 data-[state=open]:zoom-in-95",
                    className,
                )}
                ref={ref}
                {...props}
            >
                {children}
            </AlertDialogPrimitive.Content>
        </AlertDialogPortal>
    );
});

export function AlertDialogHeader({
    className,
    ...props
}: React.HTMLAttributes<HTMLDivElement>): ReactElement {
    return (
        <div
            className={cn("flex flex-col space-y-2 text-center sm:text-left", className)}
            {...props}
        />
    );
}

export function AlertDialogFooter({
    className,
    ...props
}: React.HTMLAttributes<HTMLDivElement>): ReactElement {
    return (
        <div
            className={cn("flex flex-col-reverse gap-2 sm:flex-row sm:justify-end", className)}
            {...props}
        />
    );
}

export const AlertDialogTitle = forwardRef<
    ComponentRef<typeof AlertDialogPrimitive.Title>,
    ComponentPropsWithoutRef<typeof AlertDialogPrimitive.Title>
>(function AlertDialogTitle({ className, ...props }, ref): ReactElement {
    return (
        <AlertDialogPrimitive.Title
            className={cn("text-lg font-semibold text-foreground", className)}
            ref={ref}
            {...props}
        />
    );
});

export const AlertDialogDescription = forwardRef<
    ComponentRef<typeof AlertDialogPrimitive.Description>,
    ComponentPropsWithoutRef<typeof AlertDialogPrimitive.Description>
>(function AlertDialogDescription({ className, ...props }, ref): ReactElement {
    return (
        <AlertDialogPrimitive.Description
            className={cn("text-sm text-muted-foreground", className)}
            ref={ref}
            {...props}
        />
    );
});

export const AlertDialogAction = forwardRef<
    ComponentRef<typeof AlertDialogPrimitive.Action>,
    ComponentPropsWithoutRef<typeof AlertDialogPrimitive.Action>
>(function AlertDialogAction({ className, ...props }, ref): ReactElement {
    return (
        <AlertDialogPrimitive.Action
            className={cn(buttonVariants(), className)}
            ref={ref}
            {...props}
        />
    );
});

export const AlertDialogCancel = forwardRef<
    ComponentRef<typeof AlertDialogPrimitive.Cancel>,
    ComponentPropsWithoutRef<typeof AlertDialogPrimitive.Cancel>
>(function AlertDialogCancel({ className, ...props }, ref): ReactElement {
    return (
        <AlertDialogPrimitive.Cancel
            className={cn(buttonVariants({ variant: "outline" }), className)}
            ref={ref}
            {...props}
        />
    );
});
