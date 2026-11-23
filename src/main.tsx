import { getCurrentWindow } from "@tauri-apps/api/window";
import { render } from "solid-js/web";
import App from "./App";
import Dock from "./Dock";
import { applyStoredTheme } from "./SettingsPage";
import "./styles.css";

applyStoredTheme();
window.addEventListener("storage", (event) => {
  if (event.key === "crew-theme") applyStoredTheme();
});

const isDock = getCurrentWindow().label === "dock";
document.documentElement.classList.toggle("dock-window", isDock);
render(() => (isDock ? <Dock /> : <App />), document.getElementById("root")!);
