import type { Glyph } from "../glyphs";
import { lineOnly, outlined } from "../strokes";

// Horned owl perched on a branch: ear tufts, a brow V that frames the eyes, a small beak, wings folded against its sides. Every
// part outside the face starts on the body edge and only ever moves outward, so nothing crosses the tint.
export const owl: Glyph = {
  body: (s) =>
    `<path d="M6 57.5h52M49 57.5l4-4" ${lineOnly(s, 0.55)}/>` +
    `<path d="M28 54.5v3M36 54.5v3" ${lineOnly(s)}/>` +
    `<path class="g-owl-tuft-l" d="M20 18Q13 15 9 7Q18 8 25 15" ${lineOnly(s)}/>` +
    `<path class="g-owl-tuft-r" d="M44 18Q51 15 55 7Q46 8 39 15" ${lineOnly(s)}/>` +
    `<path d="M32 14C45 14 50 23 50 34C50 46 42 54 32 54C22 54 14 46 14 34C14 23 19 14 32 14Z" ${outlined(s)}/>` +
    `<path class="g-owl-wing-l" d="M15 27C9 33 9 44 20 49.5" ${lineOnly(s, 0.6)}/>` +
    `<path class="g-owl-wing-r" d="M49 27C55 33 55 44 44 49.5" ${lineOnly(s, 0.6)}/>` +
    `<g class="g-owl-face"><path d="M19.5 23Q26 20.5 32 26Q38 20.5 44.5 23" ${lineOnly(s, 0.55)}/><path d="M30 37.5l2 3.5 2-3.5" ${lineOnly(s)}/></g>`,
  eyes: { left: 25, right: 39, y: 31, scale: 1.15 },
};
