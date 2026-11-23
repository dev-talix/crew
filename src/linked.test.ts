import { expect, test } from "bun:test";
import type { AgentHome } from "./api";
import { countdown, groupByHome, homeOffline, homeTitle, isRemote, isRemoteThread, offlineNotice, parseRemoteId, peerState, reloadsOpenChat, remoteSideLabel, runsOnOptions } from "./linked";

const studio: AgentHome = { peerId: "p1", name: "Studio", online: true };
const air: AgentHome = { peerId: "p2", name: "Air", online: false };

test("parses linked familiar ids", () => {
  expect(parseRemoteId("peer:p1:abc")).toEqual({ peerId: "p1", agentId: "abc" });
  expect(parseRemoteId("peer:p1:a:b")).toEqual({ peerId: "p1", agentId: "a:b" });
  expect(parseRemoteId("abc")).toBeNull();
  expect(parseRemoteId("peer::abc")).toBeNull();
});

test("tells linked familiars and offline Macs apart", () => {
  expect(isRemote({ home: null })).toBe(false);
  expect(isRemote({})).toBe(false);
  expect(isRemote({ home: studio })).toBe(true);
  expect(homeOffline({ home: studio })).toBe(false);
  expect(homeOffline({ home: air })).toBe(true);
  expect(homeOffline({ home: null })).toBe(false);
});

test("words the offline notice and badge title", () => {
  expect(offlineNotice({ name: "Nigel", home: air })).toBe("Air is offline, so Nigel can't be reached right now. Try again when that Mac is back.");
  expect(homeTitle({ name: "Nigel", home: studio })).toBe("Nigel lives on Studio. Open Grimoire there to change it.");
  expect(homeTitle({ name: "Nigel", home: air })).toBe("Air can't be reached right now.");
});

test("groups this Mac first, then linked Macs by name", () => {
  const groups = groupByHome([
    { id: "a", home: studio },
    { id: "b", home: null },
    { id: "c", home: air },
    { id: "d", home: studio },
  ]);
  expect(groups.map((g) => [g.name, g.online, g.agents.map((a) => a.id)])).toEqual([
    [null, true, ["b"]],
    ["Air", false, ["c"]],
    ["Studio", true, ["a", "d"]],
  ]);
  expect(groupByHome([{ id: "a", home: studio }]).map((g) => g.peerId)).toEqual(["p1"]);
});

test("says a peer's state in words", () => {
  const now = 1_000_000_000;
  expect(peerState({ online: true, lastSeen: now }, now)).toBe("Online");
  expect(peerState({ online: false, lastSeen: null }, now)).toBe("Offline");
  expect(peerState({ online: false, lastSeen: now - 2 * 3_600_000 }, now)).toBe("Offline, last seen 2 hours ago");
});

test("counts down a pairing code", () => {
  expect(countdown(65_000, 0)).toBe("1:05");
  expect(countdown(300_000, 0)).toBe("5:00");
  expect(countdown(500, 0)).toBe("0:01");
  expect(countdown(0, 0)).toBeNull();
  expect(countdown(0, 10)).toBeNull();
});

test("lists runs-on choices with offline Macs disabled", () => {
  expect(runsOnOptions([{ id: "p1", name: "Studio", online: true }, { id: "p2", name: "Air", online: false }])).toEqual([
    { value: "here", label: "This Mac" },
    { value: "peer:p1", label: "Studio", disabled: false },
    { value: "peer:p2", label: "Air (offline)", disabled: true },
    { value: "ssh", label: "Another Mac over SSH" },
  ]);
});

test("reloads the open chat only for its own linked familiar", () => {
  const remote = { id: "peer:p1:abc", home: studio };
  const local = { id: "abc", home: null };
  expect(reloadsOpenChat("peer:p1:abc", remote)).toBe(true);
  expect(reloadsOpenChat("peer:p1:other", remote)).toBe(false);
  expect(reloadsOpenChat(null, remote)).toBe(true);
  expect(reloadsOpenChat(null, local)).toBe(false);
  expect(reloadsOpenChat("abc", local)).toBe(true);
  expect(reloadsOpenChat(null, undefined)).toBe(false);
  expect(reloadsOpenChat("peer:p1:abc", null)).toBe(false);
});

test("names the home Mac on a linked familiar's side conversation rows", () => {
  expect(remoteSideLabel("Asked Scribe", { home: studio })).toBe("Asked Scribe (open it on Studio)");
  expect(remoteSideLabel("Asked Scribe", { home: null })).toBeNull();
  expect(remoteSideLabel("Asked Scribe", {})).toBeNull();
});

test("tells rows for a thread on a linked Mac from local side conversations", () => {
  expect(isRemoteThread("peer:p1:3f2c9a")).toBe(true);
  expect(isRemoteThread("3f2c9a")).toBe(false);
  expect(isRemoteThread(null)).toBe(false);
});
