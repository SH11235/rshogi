import * as DialogPrimitive from "@radix-ui/react-dialog";
import { cn } from "@shogi/design-system";
import type { ComponentPropsWithoutRef, CSSProperties, ElementRef, ReactElement } from "react";
import { forwardRef } from "react";

export const Dialog = DialogPrimitive.Root;
export const DialogTrigger = DialogPrimitive.Trigger;
export const DialogPortal = DialogPrimitive.Portal;
export const DialogClose = DialogPrimitive.Close;

export const DialogOverlay = forwardRef<
    ElementRef<typeof DialogPrimitive.Overlay>,
    ComponentPropsWithoutRef<typeof DialogPrimitive.Overlay>
>(function DialogOverlay({ className, style, ...props }, ref): ReactElement {
    return (
        <DialogPrimitive.Overlay
            style={{
                position: "fixed",
                inset: 0,
                backgroundColor: "rgba(0, 0, 0, 0.66)",
                backdropFilter: "blur(2px)",
                zIndex: 50,
                ...style,
            }}
            className={cn(
                "fixed inset-0 z-50 bg-black/70 backdrop-blur-sm data-[state=open]:animate-in data-[state=closed]:animate-out data-[state=closed]:fade-out data-[state=open]:fade-in",
                className,
            )}
            ref={ref}
            {...props}
        />
    );
});

export const DialogContent = forwardRef<
    ElementRef<typeof DialogPrimitive.Content>,
    ComponentPropsWithoutRef<typeof DialogPrimitive.Content> & {
        overlayClassName?: string;
        overlayStyle?: CSSProperties;
    }
>(function DialogContent(
    { className, children, overlayClassName, overlayStyle, style, ...props },
    ref,
): ReactElement {
    return (
        <DialogPortal>
            <DialogOverlay className={overlayClassName} style={overlayStyle} />
            <DialogPrimitive.Content
                style={{
                    position: "fixed",
                    top: "50%",
                    left: "50%",
                    transform: "translate(-50%, -50%)",
                    width: "min(960px, calc(100% - 24px))",
                    maxHeight: "90vh",
                    overflow: "auto",
                    backgroundColor: "hsl(var(--card, 0 0% 100%))",
                    color: "hsl(var(--foreground, 0 0% 10%))",
                    border: "1px solid hsl(var(--border, 0 0% 86%))",
                    borderRadius: "12px",
                    boxShadow: "0 24px 70px rgba(0, 0, 0, 0.35)",
                    padding: "24px",
                    zIndex: 51,
                    ...style,
                }}
                className={cn(
                    "fixed left-1/2 top-1/2 z-50 grid w-full max-w-lg -translate-x-1/2 -translate-y-1/2 gap-4 rounded-lg border bg-card p-6 shadow-lg duration-200 data-[state=open]:animate-in data-[state=closed]:animate-out data-[state=closed]:fade-out data-[state=open]:fade-in data-[state=closed]:zoom-out-95 data-[state=open]:zoom-in-95 data-[state=closed]:slide-out-to-left-1/2 data-[state=closed]:slide-out-to-top-1/2 data-[state=open]:slide-in-from-left-1/2 data-[state=open]:slide-in-from-top-[48%]",
                    className,
                )}
                ref={ref}
                {...props}
            >
                {children}
            </DialogPrimitive.Content>
        </DialogPortal>
    );
});

export function DialogHeader({
    className,
    ...props
}: React.HTMLAttributes<HTMLDivElement>): ReactElement {
    return (
        <div
            className={cn("flex flex-col space-y-1.5 text-center sm:text-left", className)}
            {...props}
        />
    );
}

export function DialogFooter({
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

export const DialogTitle = forwardRef<
    ElementRef<typeof DialogPrimitive.Title>,
    ComponentPropsWithoutRef<typeof DialogPrimitive.Title>
>(function DialogTitle({ className, ...props }, ref): ReactElement {
    return (
        <DialogPrimitive.Title
            className={cn(
                "text-lg font-semibold leading-none tracking-tight text-foreground",
                className,
            )}
            ref={ref}
            {...props}
        />
    );
});

export const DialogDescription = forwardRef<
    ElementRef<typeof DialogPrimitive.Description>,
    ComponentPropsWithoutRef<typeof DialogPrimitive.Description>
>(function DialogDescription({ className, ...props }, ref): ReactElement {
    return (
        <DialogPrimitive.Description
            className={cn("text-sm text-muted-foreground", className)}
            ref={ref}
            {...props}
        />
    );
});
