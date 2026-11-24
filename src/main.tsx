import { getCurrentWindow } from "@tauri-apps/api/window";
import { render } from "@solidjs/web";
import "@fontsource-variable/newsreader";
import App from "./App";
import Dock from "./Dock";
import QuickAsk from "./QuickAsk";
import { startThemes } from "./themeStore";
import "./styles.css";
import "./familiars/index.css";

startThemes();

const label = getCurrentWindow().label;
document.documentElement.classList.toggle("dock-window", label === "dock");
document.documentElement.classList.toggle("quick-window", label === "quick");
render(() => (label === "dock" ? <Dock /> : label === "quick" ? <QuickAsk /> : <App />), document.getElementById("root")!);
