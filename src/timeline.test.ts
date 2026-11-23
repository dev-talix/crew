import { describe, expect, test } from "bun:test";
import type { Message } from "./api";
import { buildRows, classifyTool, formatDuration, summarizeTools, turnInfo } from "./timeline";

const message = (id: number, role: Message["role"], text = "", createdAt = id * 1000): Message => ({
  id,
  agentId: "a",
  role,
  text,
  status: "done",
  source: null,
  createdAt,
  updatedAt: null,
});

describe("tool summaries", () => {
  test("a single tool keeps its own label", () => {
    expect(summarizeTools([classifyTool("$ ls -la")])).toBe("Ran ls -la");
    expect(summarizeTools([classifyTool("Edit src/a.rs")])).toBe("Edited src/a.rs");
    expect(summarizeTools([classifyTool("Asked Scribe")])).toBe("Asked Scribe");
  });

  test("several tools are counted by kind", () => {
    const entries = ["$ ls", "$ cat a", "Read /x/y.md", "Edit a.rs, b.rs"].map(classifyTool);
    expect(summarizeTools(entries)).toBe("Ran 2 commands, edited 2 files and read 1 file");
    expect(summarizeTools(["Grep TODO", "Glob *.ts"].map(classifyTool))).toBe("Ran 2 searches");
  });
});

describe("rows", () => {
  test("groups consecutive tool calls and reuses unchanged rows", () => {
    const list = [message(1, "user"), message(2, "tool", "$ ls"), message(3, "tool", "$ pwd"), message(4, "assistant")];
    const first = buildRows(list, new Map());
    expect(first.map((r) => r.kind)).toEqual(["user", "tools", "assistant"]);
    const second = buildRows(list, new Map(first.map((r) => [r.key, r])));
    expect(second[1]).toBe(first[1]);
    const grown = buildRows([...list.slice(0, 3), message(5, "tool", "$ id"), list[3]], new Map(first.map((r) => [r.key, r])));
    expect(grown[1]).not.toBe(first[1]);
  });

  test("marks the last reply of each turn", () => {
    const info = turnInfo([message(1, "user"), message(2, "assistant"), message(3, "tool"), message(4, "assistant"), message(5, "user")]);
    expect(info.get(2)).toEqual({ startedAt: 1000, endsTurn: false });
    expect(info.get(4)).toEqual({ startedAt: 1000, endsTurn: true });
  });
});

test("durations read like a stopwatch", () => {
  expect(formatDuration(4_200)).toBe("4s");
  expect(formatDuration(63_000)).toBe("1m 3s");
  expect(formatDuration(3_720_000)).toBe("1h 2m");
});
