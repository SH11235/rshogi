import { StrictMode } from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import "./index.css";
import { initializePositionService } from "./platform/position-service-bootstrap";

// PositionService を初期化（React レンダリング前に実行）
initializePositionService();

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
    <StrictMode>
        <App />
    </StrictMode>,
);
