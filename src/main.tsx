import { getCurrentWindow } from "@tauri-apps/api/window";
import { render } from "@solidjs/web";
import "@fontsource-variable/newsreader";
import App from "./App";
import Dock from "./Dock";
import { startThemes } from "./themeStore";
import "./styles.css";

startThemes();

const isDock = getCurrentWindow().label === "dock";
document.documentElement.classList.toggle("dock-window", isDock);
render(() => (isDock ? <Dock /> : <App />), document.getElementById("root")!);
