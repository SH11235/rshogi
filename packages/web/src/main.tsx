import React from "react";
import ReactDOM from "react-dom/client";
import { RouterProvider, createBrowserRouter } from "react-router-dom";
import App from "./App";
import "./App.css";
import { useGameStore } from "./stores/gameStore";

// Export store for debugging and testing (development only)
if (typeof window !== "undefined" && import.meta.env.DEV) {
    // biome-ignore lint/suspicious/noExplicitAny: Development only - for debugging
    (window as any).useGameStore = useGameStore;
}

const router = createBrowserRouter(
    [
        {
            path: "/",
            element: <App />,
        },
    ],
    {
        basename: import.meta.env.BASE_URL,
    },
);

const rootElement = document.getElementById("root");
if (rootElement) {
    ReactDOM.createRoot(rootElement).render(
        <React.StrictMode>
            <RouterProvider router={router} />
        </React.StrictMode>,
    );
} else {
    throw new Error("Root element with id 'root' not found.");
}
