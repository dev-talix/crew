// Writes the two app icon variants as SVG: paper (the bundle icon) and night.
// Run `bun scripts/icons.mjs`; the README's App icons section has the rest of the steps.
import { writeFileSync } from "node:fs";

const pill = (x, y, w, h, fill) => `<rect x="${x - w / 2}" y="${y - h / 2}" width="${w}" height="${h}" rx="${w / 2}" fill="${fill}"/>`;

// A wax seal's wavy rim.
function sealEdge(cx, cy, r, bumps, depth) {
  let d = "";
  for (let i = 0; i <= 360; i += 3) {
    const a = (i * Math.PI) / 180;
    const rr = r + depth * Math.sin(a * bumps) + depth * 0.4 * Math.sin(a * 7 + 1);
    d += `${i ? "L" : "M"}${(cx + rr * Math.cos(a)).toFixed(1)} ${(cy + rr * Math.sin(a)).toFixed(1)}`;
  }
  return d + "Z";
}

// The ears familiar, as strokes.
const cat = (cx, cy, s) =>
  `<path d="M${cx - 80 * s} ${cy - 46 * s} ${cx - 90 * s} ${cy - 130 * s}l${72 * s} ${50 * s}M${cx + 80 * s} ${cy - 46 * s}l${10 * s} ${-84 * s}l${-72 * s} ${50 * s}"/>` +
  `<rect x="${cx - 112 * s}" y="${cy - 98 * s}" width="${224 * s}" height="${196 * s}" rx="${80 * s}"/>`;

// Pressed into the wax: a light edge up and left, a shadow down and right.
const emboss = (shapes, light, dark, base, width) => `
  <g fill="none" stroke-linecap="round" stroke-linejoin="round" stroke-width="${width}">
    <g transform="translate(-5 -5)" stroke="${light}">${shapes}</g>
    <g transform="translate(6 6)" stroke="${dark}">${shapes}</g>
    <g stroke="${base}">${shapes}</g>
  </g>`;

const VARIANTS = {
  paper: {
    background: ["#f6eed9", "#dfcca6"],
    ribbon: "#5b3fb0",
    wax: ["#cf4252", "#9c1f30", "#701220"],
    press: ["#e0707e", "#5a0d17", "#a62536"],
  },
  night: {
    background: ["#211a30", "#0e0b16"],
    ribbon: "#7d63e0",
    wax: ["#d44a5a", "#a32334", "#6e1120"],
    press: ["#e47a87", "#520b15", "#a62536"],
  },
};

function icon({ background, ribbon, wax, press }) {
  const [light, dark, base] = press;
  return `<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024">
  <defs>
    <linearGradient id="bg" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="${background[0]}"/><stop offset="1" stop-color="${background[1]}"/></linearGradient>
    <radialGradient id="wax" cx=".36" cy=".32" r=".8"><stop offset="0" stop-color="${wax[0]}"/><stop offset=".65" stop-color="${wax[1]}"/><stop offset="1" stop-color="${wax[2]}"/></radialGradient>
    <clipPath id="tile"><rect x="100" y="100" width="824" height="824" rx="185"/></clipPath>
    <filter id="shadow" x="-20%" y="-20%" width="140%" height="140%"><feGaussianBlur stdDeviation="14"/></filter>
  </defs>
  <rect x="100" y="112" width="824" height="824" rx="185" fill="#000" opacity=".28" filter="url(#shadow)"/>
  <g clip-path="url(#tile)">
    <rect width="1024" height="1024" fill="url(#bg)"/>
    <path d="M418 660 288 968H372L470 712ZM606 660 736 968H652L554 712Z" fill="${ribbon}"/>
    <path d="${sealEdge(512, 480, 290, 9, 15)}" fill="url(#wax)"/>
    <circle cx="512" cy="480" r="218" fill="none" stroke="#6c111c" stroke-width="16" opacity=".6"/>
    ${emboss(cat(512, 500, 1.05), light, dark, base, 24)}
    ${pill(468, 500, 32, 70, dark)}${pill(556, 500, 32, 70, dark)}
  </g>
  <rect x="100" y="100" width="824" height="824" rx="185" fill="none" stroke="#fff" stroke-opacity=".12" stroke-width="4"/>
</svg>
`;
}

for (const [name, colors] of Object.entries(VARIANTS)) writeFileSync(`src/assets/icon-${name}.svg`, icon(colors));
