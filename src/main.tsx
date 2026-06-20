import React from "react";
import ReactDOM from "react-dom/client";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import App from "./App";
import Copilot from "./copilot/Copilot";
import "./styles.css";

// The cloaked overlay window (label "copilot") and the main app share one
// bundle; branch on the window label so each renders its own UI. In the plain
// Vite dev server there's no Tauri runtime, so reading the label throws —
// default to the full app there.
let label = "main";
try {
  label = getCurrentWebviewWindow().label;
} catch {
  /* no Tauri runtime (npm run dev) */
}

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    {label === "copilot" ? <Copilot /> : <App />}
  </React.StrictMode>,
);
