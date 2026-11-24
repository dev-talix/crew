import { describe, expect, test } from "bun:test";
import { CHAT_MIN, clampPanelWidth, followUntouched, OVERLAY_GAP, PANEL_DEFAULT, PANEL_MIN, parsePanelWidth } from "./panel";

describe("clampPanelWidth", () => {
  test("keeps a width that fits", () => {
    expect(clampPanelWidth(500, 1000)).toBe(500);
  });

  test("never goes below the minimum", () => {
    expect(clampPanelWidth(100, 1000)).toBe(PANEL_MIN);
  });

  test("leaves the chat its minimum", () => {
    expect(clampPanelWidth(900, 1000)).toBe(1000 - CHAT_MIN);
  });

  test("keeps the panel's minimum when the window is too narrow for both", () => {
    expect(clampPanelWidth(500, 400)).toBe(PANEL_MIN);
  });

  test("a panel floating over the chat can grow up to the overlay gap", () => {
    expect(clampPanelWidth(1000, 552, OVERLAY_GAP)).toBe(552 - OVERLAY_GAP);
  });
});

describe("parsePanelWidth", () => {
  test("reads a stored width", () => {
    expect(parsePanelWidth("512")).toBe(512);
  });

  test("falls back to the default for missing or bad values", () => {
    for (const stored of [null, "", "wide", "-4", "NaN"]) expect(parsePanelWidth(stored)).toBe(PANEL_DEFAULT);
  });
});

describe("followUntouched", () => {
  test("an untouched field takes the change made in the chat", () => {
    expect(followUntouched("full", "full", "workspace")).toBe("workspace");
  });

  test("an edited field keeps the edit", () => {
    expect(followUntouched("Wren 2", "Wren", "Wren")).toBe("Wren 2");
  });
});
