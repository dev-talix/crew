import { onCleanup } from "solid-js";
import { Portal, type JSX } from "@solidjs/web";
import { appearance } from "./themeStore";
import approvalsCss from "./approvals.css?inline";
import shieldCss from "./shield.css?inline";

const PLACES = {
  corner: "inset: auto 18px 18px auto !important; margin: 0 !important;",
  center: "inset: auto 0 18px 0 !important; margin: 0 auto !important;",
};

/**
 * Shows controls the person must be able to trust, like approval cards, where theme CSS can't reach:
 * a closed shadow root in a top-layer popover, with fixed colors. Familiars can write theme CSS.
 */
export function Shield(props: { place: keyof typeof PLACES; children: JSX.Element }) {
  const host = document.createElement("div");
  host.setAttribute("popover", "manual");
  // Inline !important beats any stylesheet rule, animation, or transition a theme can add.
  host.setAttribute(
    "style",
    "all: initial !important; display: block !important; position: fixed !important; " +
      `${PLACES[props.place]} width: min(380px, calc(100vw - 36px)) !important; height: auto !important; ` +
      "padding: 0 !important; border: 0 !important; background: transparent !important; overflow: visible !important; " +
      "opacity: 1 !important; visibility: visible !important; pointer-events: auto !important; " +
      // Only matters where popovers aren't supported, to stay above sheets.
      "z-index: 2147483647 !important;",
  );
  const layer = document.createElement("div");
  host.attachShadow({ mode: "closed" }).append(layer);
  // Outside body, so a theme that hides body can't take the shield with it.
  document.documentElement.append(host);
  host.showPopover?.();
  onCleanup(() => host.remove());
  return (
    <Portal mount={layer}>
      <style>{shieldCss + approvalsCss}</style>
      <div class={["shield", appearance()]}>{props.children}</div>
    </Portal>
  );
}
