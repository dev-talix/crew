import { createSignal, For, onCleanup, Show } from "solid-js";
import { api, errorText } from "./api";
import { Sheet, Toggle } from "./ui";
import { canImport, groupPreview, importedSummary, previewSummary, type ExportKind, type Picked } from "./transfer";
import "./transfer.css";

/** Saves one familiar, spell, coven, or theme as a .grimoire.json file. */
export function ExportButton(props: { kind: ExportKind; id: string; onError: (message: string) => void }) {
  const [state, setState] = createSignal<"idle" | "busy" | "done">("idle");
  let timer: number | undefined;
  onCleanup(() => clearTimeout(timer));
  const run = async () => {
    setState("busy");
    try {
      const path = await api.exportItem(props.kind, props.id);
      setState(path ? "done" : "idle");
      if (path) timer = window.setTimeout(() => setState("idle"), 2500);
    } catch (e) {
      setState("idle");
      props.onError(errorText(e));
    }
  };
  return (
    <button type="button" class="ghost" disabled={state() === "busy"} onClick={() => void run()}>
      <span aria-live="polite">{state() === "done" ? "Exported" : "Export"}</span>
    </button>
  );
}

/** Settings, General: back everything up to one file, or import a backup or a shared file. */
export function BackupSection() {
  const [history, setHistory] = createSignal(false);
  const [busy, setBusy] = createSignal(false);
  const [message, setMessage] = createSignal("");
  const [error, setError] = createSignal("");
  const [picked, setPicked] = createSignal<Picked | null>(null);

  const run = async (action: () => Promise<void>) => {
    setBusy(true);
    setError("");
    setMessage("");
    try {
      await action();
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(false);
    }
  };

  const exportBackup = () =>
    run(async () => {
      const path = await api.exportBackup(history());
      if (path) setMessage(`Saved the backup to ${path.replace(/^\/Users\/[^/]+/, "~")}.`);
    });

  const pick = () =>
    run(async () => {
      setPicked(await api.pickImport());
    });

  return (
    <>
      <h2 class="section-title">Backup</h2>
      <div class="list">
        <div class="list-row settings-row">
          <div class="list-main">
            <div class="list-title">Export a backup</div>
            <p class="settings-detail">Familiars, spells, rituals, covens, your themes, and settings in one file. Vault items and secrets stay in your Keychain.</p>
            <label class="transfer-check">
              <input type="checkbox" checked={history()} onChange={(e) => setHistory(e.currentTarget.checked)} />
              Include chat history
            </label>
          </div>
          <button type="button" class="ghost small" disabled={busy()} onClick={() => void exportBackup()}>
            Export a backup
          </button>
        </div>
        <div class="list-row settings-row">
          <div class="list-main">
            <div class="list-title">Import</div>
            <p class="settings-detail">A backup, or a familiar, spell, coven, or theme someone shared. You see what it adds before anything changes.</p>
          </div>
          <button type="button" class="ghost small" disabled={busy()} onClick={() => void pick()}>
            Import
          </button>
        </div>
      </div>
      <Show when={message()}>
        <p class="hint settings-footnote" role="status">
          {message()}
        </p>
      </Show>
      <Show when={error()}>
        <div class="error-box settings-error" role="alert">
          {error()}
        </div>
      </Show>
      <Show when={picked()}>
        {(p) => (
          <ImportSheet
            picked={p()}
            onClose={() => setPicked(null)}
            onDone={(summary) => {
              setPicked(null);
              setMessage(summary);
            }}
          />
        )}
      </Show>
    </>
  );
}

const TAG = { conflict: "Renamed", skipped: "Can't import", fresh: "New", replaces: "Replaces" };

/** What an import adds, with conflicts and how they resolve, before anything is written. */
export function ImportSheet(props: { picked: Picked; onClose: () => void; onDone: (summary: string) => void }) {
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal("");
  const [keepFullAccess, setKeepFullAccess] = createSignal(false);
  const preview = () => props.picked.preview;

  const apply = async (e: Event) => {
    e.preventDefault();
    setBusy(true);
    setError("");
    try {
      props.onDone(importedSummary(await api.applyImport(keepFullAccess())));
    } catch (err) {
      setError(errorText(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Sheet label="Import" wide onClose={props.onClose}>
      <form class="wf-form import-preview" onSubmit={apply}>
        <h1>Import {props.picked.file}</h1>
        <p class="hint">{previewSummary(preview())}</p>
        <For each={groupPreview(preview())}>
          {(group) => (
            <section class="import-group">
              <h2 class="section-title">{group.title}</h2>
              <ul class="list">
                <For each={group.items}>
                  {(item) => {
                    const tag = item.skipped ? "skipped" : item.what === "settings" ? "replaces" : item.conflict ? "conflict" : "fresh";
                    return (
                      <li class={["list-row import-item", { skipped: item.skipped }]}>
                        <div class="list-main">
                          <div class="list-title">
                            {item.name}
                            <Show when={item.conflict && !item.skipped}>
                              <span class="import-rename"> becomes {item.finalName}</span>
                            </Show>
                          </div>
                          <For each={item.notes}>{(note) => <p class="list-sub">{note}</p>}</For>
                        </div>
                        <span class="import-tag" data-tag={tag}>
                          {TAG[tag]}
                        </span>
                      </li>
                    );
                  }}
                </For>
              </ul>
            </section>
          )}
        </For>
        <Show when={preview().fullAccess}>
          <div class="list">
            <div class="list-row">
              <div class="list-main">
                <div class="list-title">Keep full access</div>
                <p class="list-sub">
                  This file gives a familiar full access: commands run anywhere on this Mac, without a sandbox. Off, it gets the same access as "Workspace only" when you summon a familiar.
                </p>
              </div>
              <Toggle label="Keep full access" checked={keepFullAccess()} onChange={setKeepFullAccess} />
            </div>
          </div>
        </Show>
        <p class="hint">Everything gets a new id, and nothing you have now changes, except settings and the covens that familiars move into.</p>
        <Show when={error()}>
          <p class="error-box" role="alert">
            {error()}
          </p>
        </Show>
        <div class="form-actions">
          <span class="spacer" />
          <button type="button" class="ghost" onClick={props.onClose}>
            Cancel
          </button>
          <button class="primary" type="submit" disabled={busy() || !canImport(preview())}>
            Import
          </button>
        </div>
      </form>
    </Sheet>
  );
}
