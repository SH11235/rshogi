import preset from "@shogi/design-system/tailwind.preset";
import type { Config } from "tailwindcss";

export default {
    presets: [preset],
    content: ["./src/**/*.{ts,tsx}", "./index.html", "../../packages/ui/src/**/*.{ts,tsx}"],
} satisfies Config;
