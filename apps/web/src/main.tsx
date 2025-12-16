import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "./index.css";
import App from "./App";
import { initializePositionService } from "./platform/position-service-bootstrap";

// PositionService を初期化（React レンダリング前に実行）
initializePositionService();

const rootElement = document.getElementById("root");

if (!rootElement) {
    throw new Error("Root element not found");
}

createRoot(rootElement).render(
    <StrictMode>
        <App />
    </StrictMode>,
);
