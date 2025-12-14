import preset from "@shogi/design-system/tailwind.preset";
import type { Config } from "tailwindcss";

export default {
    presets: [preset],
    content: ["./index.html", "./src/**/*.{ts,tsx}", "../../packages/ui/src/**/*.{ts,tsx}"],
} satisfies Config;
