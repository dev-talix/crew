import { expect, test } from "bun:test";
import { createRoot, createSignal } from "solid-js";
import { createFetch, describeSchedule, hostState, onMountAsync, withAgent } from "./api";

test("describeSchedule names clocks and event triggers", () => {
  expect(describeSchedule({ kind: "interval", minutes: 120 })).toBe("Every 2 hours");
  expect(describeSchedule({ kind: "interval", minutes: 45 })).toBe("Every 45 minutes");
  expect(describeSchedule({ kind: "githubPr", repo: "openai/codex" })).toBe("When a PR opens in openai/codex");
  expect(describeSchedule({ kind: "file", path: "/Users/ada/Notes" })).toBe("When ~/Notes changes");
  expect(describeSchedule({ kind: "webhook", secret: "abc" })).toBe("When its webhook is called");
  expect(describeSchedule({ kind: "email" })).toBe("When new email arrives");
});

const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((res, rej) => ((resolve = res), (reject = rej)));
  return { promise, resolve, reject };
}

test("createFetch loads, ignores a stale result, and reports failure as null", async () => {
  const requests = new Map<string, ReturnType<typeof deferred<string>>>();
  const { value, setSource, dispose } = createRoot((dispose) => {
    const [source, setSource] = createSignal("codex");
    const value = createFetch(source, (s) => {
      const request = deferred<string>();
      requests.set(s, request);
      return request.promise;
    });
    return { value, setSource, dispose };
  });

  await settle();
  expect(value()).toBeUndefined();

  setSource("claude");
  await settle();
  requests.get("codex")!.resolve("codex models");
  await settle();
  expect(value()).toBeUndefined();

  requests.get("claude")!.resolve("claude models");
  await settle();
  expect(value()).toBe("claude models");

  setSource("broken");
  await settle();
  requests.get("broken")!.reject(new Error("offline"));
  await settle();
  expect(value()).toBeNull();
  dispose();
});

test("onMountAsync stops listeners that arrive after disposal", async () => {
  const stopped: string[] = [];
  const late = deferred<() => void>();
  const dispose = createRoot((dispose) => {
    onMountAsync(async (keep) => {
      keep(() => stopped.push("early"));
      keep(await late.promise);
    });
    return dispose;
  });

  await settle();
  dispose();
  expect(stopped).toEqual(["early"]);

  late.resolve(() => stopped.push("late"));
  await settle();
  expect(stopped).toEqual(["early", "late"]);
});

test("hostState says a remote familiar's Mac is offline or owes it its setup", () => {
  expect(hostState({})).toBe(null);
  expect(hostState({ offline: true })).toBe("Offline");
  expect(hostState({ setupPending: true, offline: true })).toBe("Setup pending");
  expect(hostState({ setupPending: false, offline: false })).toBe(null);
});

test("withAgent adds a saved familiar once, keeping the listed copy", () => {
  const listed = [{ id: "a" }, { id: "b", setupPending: false }];
  // The form saved "b" while its Mac was offline; the list refreshed after setup finished.
  expect(withAgent(listed, { id: "b", setupPending: true })).toBe(listed);
  expect(withAgent(listed, { id: "c" })).toEqual([{ id: "a" }, { id: "b", setupPending: false }, { id: "c" }]);
});
