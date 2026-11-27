import { describe, expect, test } from "bun:test";
import { enabledLabel, nameFrom, skillRowProblem, sourceLabel, templateSkills, validName, type Skill } from "./skills";

const skill = (over: Partial<Skill>): Skill => ({
  name: "brief",
  description: "Short briefs.",
  source: "you",
  origin: null,
  createdBy: null,
  agents: [],
  files: ["SKILL.md"],
  updatedAt: 1,
  ...over,
});

test("names follow the agentskills.io rules", () => {
  for (const good of ["a", "pdf-forms", "v2-notes", "x".repeat(64)]) expect(validName(good)).toBe(true);
  for (const bad of ["", "Caps", "-lead", "trail-", "two--dashes", "under_score", "x".repeat(65)]) expect(validName(bad)).toBe(false);
  expect(nameFrom("  Haggle Setup!! v2 ")).toBe("haggle-setup-v2");
  expect(nameFrom("???")).toBe("skill");
  expect(nameFrom(`${"a".repeat(63)} b`)).toBe("a".repeat(63));
});

test("labels say who made a skill and who has it on", () => {
  const names = { a1: "Ada", b2: "Bo", c3: "Cy" };
  expect(sourceLabel(skill({ source: "familiar", createdBy: "a1" }), names)).toBe("Made by Ada");
  expect(sourceLabel(skill({ source: "familiar", createdBy: "gone" }), names)).toBe("Made by a deleted familiar");
  expect(sourceLabel(skill({ source: "url", origin: "https://github.com/a/b" }), names)).toBe("From github.com/a/b");
  expect(sourceLabel(skill({}), names)).toBe("Added by you");
  expect(enabledLabel(skill({}), names)).toBe("Off for every familiar");
  expect(enabledLabel(skill({ agents: ["a1"] }), names)).toBe("On for Ada");
  expect(enabledLabel(skill({ agents: ["a1", "b2", "c3"] }), names)).toBe("On for Ada, Bo and Cy");
});

describe("templateSkills", () => {
  test("turns each template skill into an editable skill row", () => {
    const rows = templateSkills([
      { name: "Haggle Setup", description: "Run a first-run interview. Then save answers." },
      { name: "", description: "Untitled one." },
      { name: "blank", description: "   " },
    ]);
    expect(rows.map((r) => r.name)).toEqual(["haggle-setup", "skill-2"]);
    expect(rows[0].description).toBe("Use when asked to Haggle Setup. Run a first-run interview.");
    expect(rows[0].body).toBe("Run a first-run interview. Then save answers.");
    expect(rows[1].description).toBe("Untitled one.");
    expect(rows.every((r) => skillRowProblem(r) === null)).toBe(true);
  });

  test("flags rows the backend would refuse", () => {
    const [row] = templateSkills([{ name: "ok", description: "Fine." }]);
    expect(skillRowProblem({ ...row, name: "Bad Name" })).toContain("lowercase");
    expect(skillRowProblem({ ...row, description: " " })).toContain("when to use");
    expect(skillRowProblem({ ...row, description: "x".repeat(1025) })).toContain("1024");
    expect(skillRowProblem({ ...row, body: "" })).toContain("instructions");
  });
});
