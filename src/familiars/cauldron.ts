import type { Glyph } from "../glyphs";
import { lineOnly, outlined } from "../strokes";

// A squat pot on two stubby legs with a thick rim. The belly path stays open at the top so its
// closing edge doesn't show through the rim. Bubbles, steam, the puff ring, and the spill are
// hidden until cauldron.css animates them.
export const cauldron: Glyph = {
  body: (s) =>
    `<path class="g-cauldron-steam" d="M26 13c-3-3 2.5-5-.5-9" ${lineOnly(s, 0.55)}/>` +
    `<path class="g-cauldron-steam" d="M38 12c3-3-2.5-5 .5-9" ${lineOnly(s, 0.55)}/>` +
    `<ellipse class="g-cauldron-ring" cx="32" cy="11" rx="7" ry="2.6" ${lineOnly(s, 0.8)}/>` +
    `<circle class="g-cauldron-bubble" cx="27" cy="14" r="2.6" ${lineOnly(s)}/>` +
    `<circle class="g-cauldron-bubble" cx="37" cy="13" r="2.2" ${lineOnly(s)}/>` +
    `<circle class="g-cauldron-bubble" cx="32" cy="15" r="1.7" ${lineOnly(s)}/>` +
    `<path d="M23 53 20.5 58.5M41 53 43.5 58.5" ${lineOnly(s)}/>` +
    `<path d="M12 28C8 38 11 50 22 53h20c11-3 14-15 10-25" ${outlined(s)}/>` +
    `<path class="g-cauldron-drip" d="M44.5 28v4.5a1.7 1.7 0 0 0 3.4 0V28M18 28v2a1.4 1.4 0 0 0 2.8 0v-2" ${lineOnly(s, 0.8)}/>` +
    `<rect class="g-cauldron-rim" x="8" y="19" width="48" height="9" rx="4.5" ${outlined(s)}/>`,
  eyes: { left: 25, right: 39, y: 39, scale: 1 },
};
