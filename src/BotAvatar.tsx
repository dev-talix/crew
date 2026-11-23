import { createMemo, createSignal, Match, onCleanup, onMount, Show, Switch } from "solid-js";
import { onCue } from "./animator";
import { luminance, shade, type Avatar, type Eyes } from "./avatar";
import { GLYPHS } from "./glyphs";

export type Mood = "idle" | "working" | "happy" | "concerned";

const reducedMotion = () => window.matchMedia("(prefers-reduced-motion: reduce)").matches;
const BLINK_MS = 190;
const GLANCE_MS = 1300;
const FIDGET_MS = 700;
const WINK_MS = 420;

function Eye(props: { x: number; y: number; k: number; kind: Eyes; mood: Mood; color: string; side: -1 | 1 }) {
  const k = () => props.k;
  return (
    <Switch>
      <Match when={props.mood === "happy"}>
        <path
          class="eye-mood"
          d={`M${props.x - 3.6 * k()} ${props.y + 1.6 * k()}q${3.6 * k()} ${-5.6 * k()} ${7.2 * k()} 0`}
          stroke={props.color}
          stroke-width={3.2 * k()}
          stroke-linecap="round"
          fill="none"
        />
      </Match>
      <Match when={props.mood === "concerned"}>
        {/* Rotate on a plain group: CSS transform-origin on .eye-mood would skew an attribute rotate. */}
        <g transform={`rotate(${props.side * 18} ${props.x} ${props.y + 0.8 * k()})`}>
          <rect
            class="eye-mood"
            x={props.x - 3.6 * k()}
            y={props.y - 0.8 * k()}
            width={7.2 * k()}
            height={3.2 * k()}
            rx={1.6 * k()}
            fill={props.color}
          />
        </g>
      </Match>
      <Match when={props.kind === "dot"}>
        <circle cx={props.x} cy={props.y} r={3 * k()} fill={props.color} />
      </Match>
      <Match when={props.kind === "slit"}>
        <rect x={props.x - 3.5 * k()} y={props.y - 1.75 * k()} width={7 * k()} height={3.5 * k()} rx={1.75 * k()} fill={props.color} />
      </Match>
      <Match when={true}>
        <rect x={props.x - 2.25 * k()} y={props.y - 4 * k()} width={4.5 * k()} height={8 * k()} rx={2.25 * k()} fill={props.color} />
      </Match>
    </Switch>
  );
}

export default function BotAvatar(props: {
  avatar: Avatar;
  size?: number;
  working?: boolean;
  mood?: Mood;
  fluid?: boolean;
  alive?: boolean;
  label?: string;
}) {
  const size = () => props.size ?? 32;
  const stroke = () => (size() >= 72 ? 3 : size() >= 40 ? 2.4 : 1.8);
  const deep = createMemo(() => luminance(props.avatar.color) < 0.08);
  const color = createMemo(() => (luminance(props.avatar.color) > 0.4 ? shade(props.avatar.color, -0.2) : props.avatar.color));
  const glyph = createMemo(() => GLYPHS[props.avatar.shape]);
  const body = createMemo(() => glyph().body({ color: color(), stroke: stroke() }));
  const mood = (): Mood => props.mood ?? (props.working ? "working" : "idle");
  const floating = () => mood() === "working" && !reducedMotion();

  const [blinking, setBlinking] = createSignal(false);
  const [glance, setGlance] = createSignal(0);
  const [fidget, setFidget] = createSignal(false);
  const [winking, setWinking] = createSignal(false);
  let timers: number[] = [];
  const later = (fn: () => void, ms: number) => timers.push(window.setTimeout(fn, ms));

  onMount(() => {
    if (props.alive === false) return;
    const stop = onCue((cue) => {
      if (mood() === "happy" || mood() === "concerned") return;
      if (cue === "blink") {
        setBlinking(true);
        later(() => setBlinking(false), BLINK_MS);
      } else if (mood() !== "idle") {
        return;
      } else if (cue === "wink") {
        setWinking(true);
        later(() => setWinking(false), WINK_MS);
      } else if (cue === "fidget") {
        setFidget(true);
        later(() => setFidget(false), FIDGET_MS);
      } else if (glance() === 0) {
        setGlance(cue === "glance-left" ? -1 : 1);
        later(() => setGlance(0), GLANCE_MS);
      }
    });
    onCleanup(stop);
  });
  onCleanup(() => timers.forEach(clearTimeout));

  return (
    <svg
      class={`bot-avatar shape-${props.avatar.shape} mood-${mood()}`}
      classList={{ deep: deep(), fidget: fidget(), "glance-l": glance() < 0, "glance-r": glance() > 0 }}
      width={size()}
      height={size()}
      viewBox="-4 -4 72 72"
      role={props.label ? "img" : undefined}
      aria-label={props.label}
      aria-hidden={props.label ? undefined : "true"}
    >
      <g>
        <Show when={floating()}>
          <animateTransform
            attributeName="transform"
            type="translate"
            dur="2.6s"
            repeatCount="indefinite"
            values="0 0;0 -2.2;0 0"
            keyTimes="0;0.5;1"
            calcMode="spline"
            keySplines="0.45 0 0.55 1;0.45 0 0.55 1"
          />
        </Show>
        <Show when={glyph()} keyed>
          {(_shape) => (
            <g class="bot-glyph" classList={{ pop: !!props.fluid }}>
              <g class="bot-body">
                <g innerHTML={body()} />
                <g
                  class="bot-eyes"
                  classList={{ scanning: mood() === "working" }}
                  style={{ transform: `translateX(${glance() * 2.4}px)` }}
                >
                  <g class="bot-eye" classList={{ blink: blinking() }}>
                    <Eye
                      x={glyph().eyes.left}
                      y={glyph().eyes.y}
                      k={glyph().eyes.scale}
                      kind={props.avatar.eyes}
                      mood={mood()}
                      color={color()}
                      side={-1}
                    />
                  </g>
                  <g class="bot-eye wink-eye" classList={{ blink: blinking(), wink: winking() }}>
                    <Eye
                      x={glyph().eyes.right}
                      y={glyph().eyes.y}
                      k={glyph().eyes.scale}
                      kind={props.avatar.eyes}
                      mood={mood()}
                      color={color()}
                      side={1}
                    />
                  </g>
                </g>
              </g>
            </g>
          )}
        </Show>
      </g>
    </svg>
  );
}
