import { createSignal, For, onCleanup } from "solid-js";
import BotAvatar, { type Mood } from "./BotAvatar";
import { EYES, PALETTE, SHAPES, isHexColor, randomAvatar, type Avatar, type PartOption } from "./avatar";
import { ShuffleIcon } from "./icons";

type PartKey = "shape" | "eyes";

function PartRow<K extends PartKey>(props: {
  title: string;
  part: K;
  options: PartOption<Avatar[K]>[];
  avatar: Avatar;
  onChange: (avatar: Avatar) => void;
}) {
  return (
    <div class="studio-row" role="radiogroup" aria-label={props.title}>
      <span class="studio-label">{props.title}</span>
      <div class="studio-options">
        <For each={props.options}>
          {(option) => {
            const variant = (): Avatar => ({ ...props.avatar, [props.part]: option.value });
            const selected = () => props.avatar[props.part] === option.value;
            return (
              <button
                type="button"
                role="radio"
                class={["studio-option", { selected: selected() }]}
                aria-checked={selected() ? "true" : "false"}
                aria-label={`${props.title}: ${option.label}`}
                title={option.label}
                onClick={() => props.onChange(variant())}
              >
                <BotAvatar avatar={variant()} size={28} alive={false} />
              </button>
            );
          }}
        </For>
      </div>
    </div>
  );
}

const PREVIEW_MOODS: Mood[] = ["idle", "happy", "idle", "working", "idle", "concerned"];

export default function AvatarStudio(props: { avatar: Avatar; name: string; onChange: (avatar: Avatar) => void }) {
  const [step, setStep] = createSignal(0);
  const cycle = setInterval(() => setStep((n) => (n + 1) % PREVIEW_MOODS.length), 2400);
  onCleanup(() => clearInterval(cycle));
  const previewMood = () => PREVIEW_MOODS[step()];
  const custom = () => !PALETTE.some((p) => p.value === props.avatar.color);
  const setColor = (color: string) => isHexColor(color) && props.onChange({ ...props.avatar, color: color.toLowerCase() });

  return (
    <div class="studio">
      <div class="studio-stage">
        <BotAvatar avatar={props.avatar} size={112} fluid mood={previewMood()} label={`${props.name || "New familiar"} avatar`} />
        <button type="button" class="ghost small" onClick={() => props.onChange(randomAvatar())}>
          <ShuffleIcon /> Shuffle
        </button>
      </div>
      <div class="studio-controls">
        <PartRow title="Shape" part="shape" options={SHAPES} avatar={props.avatar} onChange={props.onChange} />
        <PartRow title="Eyes" part="eyes" options={EYES} avatar={props.avatar} onChange={props.onChange} />
        <div class="studio-row" role="radiogroup" aria-label="Color">
          <span class="studio-label">Color</span>
          <div class="studio-options swatches">
            <For each={PALETTE}>
              {(swatch) => (
                <button
                  type="button"
                  role="radio"
                  class={["swatch", { selected: props.avatar.color === swatch.value }]}
                  aria-checked={props.avatar.color === swatch.value ? "true" : "false"}
                  aria-label={`Color: ${swatch.label}`}
                  title={swatch.label}
                  style={{ "--swatch": swatch.value }}
                  onClick={() => setColor(swatch.value)}
                />
              )}
            </For>
            <label
              class={["swatch custom", { selected: custom() }]}
              title="Pick any color"
              style={{ "--swatch": custom() ? props.avatar.color : "transparent" }}
            >
              <input
                type="color"
                aria-label="Custom color"
                value={props.avatar.color}
                onInput={(e) => setColor(e.currentTarget.value)}
              />
            </label>
          </div>
        </div>
      </div>
    </div>
  );
}
