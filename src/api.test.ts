import { expect, test } from "bun:test";
import { createRoot, createSignal } from "solid-js";
import { createFetch, onMountAsync } from "./api";

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
