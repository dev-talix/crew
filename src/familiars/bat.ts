import type { Glyph } from "../glyphs";
import { lineOnly, outlined } from "../strokes";

// A round bat. Head and ears are one outline. Each scalloped wing touches the head at a single
// shoulder point (19.3 33 / 44.7 33) and hinges there, so in open poses it stays clear of the head.
// When concerned the wings wrap in front of the lower head like a cloak. The head then swaps to
// g-bat-head-cut, the same outline with the wrapped wings' area cut out along their leading edges,
// so no head line or second tint shows through. The cut path matches the pose in bat.css
// (translate(0, 5px) rotate(∓100deg) scale(0.85, -0.8) about the shoulder); change both together.
export const bat: Glyph = {
  body: (s) =>
    `<path class="g-bat-head" d="M23 26.6A13 13 0 1 0 41 26.6L43 15 35 23.35A13 13 0 0 0 29 23.35L21 15z" ${outlined(s)}/>` +
    `<path class="g-bat-head-cut" d="M23 26.6A13 13 0 0 0 19.2 38.27Q19.25 38.13 19.3 38Q30.08 42.4 31.19 48.97A13 13 0 0 0 32.81 48.97Q33.92 42.4 44.7 38Q44.75 38.13 44.8 38.27A13 13 0 0 0 41 26.6L43 15 35 23.35A13 13 0 0 0 29 23.35L21 15z" ${outlined(s)}/>` +
    `<path class="g-bat-mouth" d="M29.5 43.2l1 1.8 1-1.8M32.5 43.2l1 1.8 1-1.8" ${lineOnly(s, 0.6)}/>` +
    `<path class="g-bat-wing-l" d="M19.3 33Q11 19 2 21L4 33Q7.5 29.5 9.5 36Q12.5 33 14.5 39.5Q16 35 19.3 33z" ${outlined(s)}/>` +
    `<path class="g-bat-wing-r" d="M44.7 33Q53 19 62 21L60 33Q56.5 29.5 54.5 36Q51.5 33 49.5 39.5Q48 35 44.7 33z" ${outlined(s)}/>`,
  eyes: { left: 27, right: 37, y: 36, scale: 0.8 },
};
