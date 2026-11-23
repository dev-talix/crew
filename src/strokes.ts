// Stroke attributes shared by every glyph. Kept apart from glyphs.ts so a shape file imports them
// without pulling in the glyph registry.
export interface GlyphStyle {
  color: string;
  stroke: number;
}

export const outlined = (s: GlyphStyle) =>
  `stroke="${s.color}" stroke-width="${s.stroke}" vector-effect="non-scaling-stroke" stroke-linecap="round" stroke-linejoin="round" fill="${s.color}" fill-opacity="0.12"`;

export const lineOnly = (s: GlyphStyle, opacity = 1) =>
  `stroke="${s.color}" stroke-opacity="${opacity}" stroke-width="${s.stroke}" vector-effect="non-scaling-stroke" stroke-linecap="round" stroke-linejoin="round" fill="none"`;
