#!/usr/bin/env node

import { exec, execSync } from "node:child_process";
import { platform } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const __filename = fileURLToPath(import.meta.url);
const __dirname = dirname(__filename);

const viewerPath = join(__dirname, "viewer.html");

// コマンドをOSに応じて選択
let command;

// WSL環境の検出
const isWSL = process.platform === "linux" && (process.env.WSL_DISTRO_NAME || process.env.WSLENV);

if (isWSL) {
    console.log("WSL environment detected. Using Windows explorer.");
    try {
        // wslpathでWindowsパスに変換
        const windowsPath = execSync(`wslpath -w "${viewerPath}"`, { encoding: "utf8" }).trim();
        command = `explorer.exe "${windowsPath}"`;
    } catch (e) {
        console.error("Failed to convert WSL path:", e);
        // フォールバック：直接explorer.exeを使用
        command = `explorer.exe "${viewerPath}"`;
    }
} else {
    switch (platform()) {
        case "darwin":
            command = `open "${viewerPath}"`;
            break;
        case "win32":
            command = `start "${viewerPath}"`;
            break;
        default:
            // Linux and others
            command = `xdg-open "${viewerPath}" || sensible-browser "${viewerPath}" || x-www-browser "${viewerPath}"`;
    }
}

console.log("Opening benchmark viewer...");
console.log(`File path: ${viewerPath}`);

exec(command, (error) => {
    if (error) {
        console.error("Failed to open viewer:", error);
        console.log(`Please open the following file manually: ${viewerPath}`);

        if (isWSL) {
            console.log("\nWSL alternative methods:");
            console.log("1. Copy and paste this command:");
            console.log(`   explorer.exe $(wslpath -w "${viewerPath}")`);
            console.log("\n2. Or start a local server:");
            console.log("   cd packages/core/benchmarks && npx http-server -p 8000");
            console.log("   Then open http://localhost:8000/viewer.html in your browser");
        }
    } else {
        console.log("Viewer opened successfully!");
    }
    // Exit after a short delay to ensure the command completes
    setTimeout(() => process.exit(0), 1000);
});
