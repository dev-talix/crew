import { expect, test } from "bun:test";
import { ledgerIsBlank, ledgerUpdated, type Ledger } from "./ledgers";

const blank: Ledger = { goal: "", status: "idle", waitingOn: "", nextStep: "", updatedAt: null, updatedBy: null };

test("a ledger is blank until it has a goal, status, or notes", () => {
  expect(ledgerIsBlank(blank)).toBe(true);
  expect(ledgerIsBlank({ ...blank, status: "done" })).toBe(false);
  expect(ledgerIsBlank({ ...blank, nextStep: "Run the tests" })).toBe(false);
});

test("names who last updated the ledger", () => {
  const now = 1_800_000_000_000;
  expect(ledgerUpdated(blank, "Scribe", now)).toBe("");
  expect(ledgerUpdated({ updatedAt: now - 5 * 60_000, updatedBy: "familiar" }, "Scribe", now)).toBe("Updated by Scribe 5 minutes ago");
  expect(ledgerUpdated({ updatedAt: now - 10_000, updatedBy: "you" }, "Scribe", now)).toBe("Updated by you just now");
});
