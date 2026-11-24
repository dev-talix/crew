import { Show } from "solid-js";
import BotAvatar from "../BotAvatar";
import type { Familiar } from "./api";

/** The familiar's avatar, still. BotAvatar and its imports are free of Tauri, so the phone reuses it. */
export function FamiliarFace(props: { familiar?: Familiar; size: number; working?: boolean }) {
  return (
    <Show when={props.familiar} fallback={<span class="face-blank" style={{ width: `${props.size}px`, height: `${props.size}px` }} />}>
      {(f) => <BotAvatar avatar={f().avatar} size={props.size} alive={false} working={props.working} label={f().name} />}
    </Show>
  );
}
