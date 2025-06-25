import React from "react";
import ReactDOM from "react-dom/client";
import { RouterProvider, createBrowserRouter } from "react-router-dom";
import App from "./App";
import { P2PTestPage } from "./pages/P2PTestPage";
import "./App.css";

const router = createBrowserRouter([
    {
        path: "/",
        element: <App />,
    },
    {
        path: "/p2p-test",
        element: <P2PTestPage />,
    },
]);

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
