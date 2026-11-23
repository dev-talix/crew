import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { SHAPES } from "./avatar";
import { GLYPHS } from "./glyphs";

test("every shape has a glyph that draws", () => {
  for (const { value } of SHAPES) {
    const markup = GLYPHS[value].body({ color: "#4c7ef3", stroke: 2 });
    expect(markup).toContain("#4c7ef3");
  }
});

test("the backend accepts the same shapes the studio offers", () => {
  const db = readFileSync(new URL("../src-tauri/src/db.rs", import.meta.url), "utf8");
  const list = db.match(/pub const SHAPES: &\[&str\] = &\[([^\]]*)\]/)?.[1] ?? "";
  const backend = [...list.matchAll(/"([a-z]+)"/g)].map((m) => m[1]);
  expect(backend).toEqual(SHAPES.map((s) => s.value));
});
