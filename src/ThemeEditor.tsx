import { createEffect, createMemo, createSignal, createStore, For, onCleanup, onSettled, Show } from "solid-js";
import { api, errorText, type Agent, type ThemeEntry } from "./api";
import { mix, modesOf, palette, ROLE_LABELS, ROLES, seeds, type Colors, type Mode, type Role, type ThemeFile } from "./theme";
import { activeTheme, previewMode, previewTheme, themes } from "./themeStore";
import { Segmented, Sheet } from "./ui";
import { Shield } from "./Shield";
import { ExportButton } from "./ImportExport";
import "./themes.css";

const MODES: Mode[] = ["dark", "light"];
const SEEDS: Role[] = ["canvas", "accent"];
const HEX = /^#([0-9a-f]{3}|[0-9a-f]{6})$/i;

/** The appearance a theme shows in: the one on screen when it has it, else the one it has. */
const modeFor = (theme: ThemeFile, onScreen: Mode) => (modesOf(theme).includes(onScreen) ? onScreen : modesOf(theme)[0]);

function useSystemDark() {
  const query = window.matchMedia("(prefers-color-scheme: dark)");
  const [dark, setDark] = createSignal(query.matches);
  onSettled(() => {
    const change = () => setDark(query.matches);
    query.addEventListener("change", change);
    return () => query.removeEventListener("change", change);
  });
  return dark;
}

/** Theme cards for Settings, plus the editor sheet. `mode` is the mode setting. */
export function ThemePicker(props: { mode: "system" | "light" | "dark"; agents: Agent[] }) {
  const systemDark = useSystemDark();
  const onScreen = (): Mode => (props.mode === "system" ? (systemDark() ? "dark" : "light") : props.mode);
  const [editing, setEditing] = createSignal<{ id: string | null; theme: ThemeFile } | null>(null);
  const [error, setError] = createSignal("");
  const active = () => activeTheme();
  const nameOf = (id: string) => props.agents.find((a) => a.id === id)?.name ?? "a deleted familiar";

  // A familiar's theme can carry CSS, so it shows as a preview until the person applies it.
  const [trying, setTrying] = createSignal<ThemeEntry | null>(null);
  const stopTrying = () => {
    setTrying(null);
    previewTheme(null);
  };
  onCleanup(() => trying() && previewTheme(null));

  const apply = async (entry: ThemeEntry) => {
    setError("");
    try {
      await api.setTheme(entry.id);
    } catch (e) {
      setError(errorText(e));
    }
  };

  const choose = (entry: ThemeEntry) => {
    if (!entry.theme.createdBy || themes()?.active === entry.id) {
      if (trying()) stopTrying();
      return void apply(entry);
    }
    setTrying(entry);
    previewTheme(entry.theme);
  };

  const edit = () => {
    if (trying()) stopTrying();
    const entry = active();
    if (!entry) return;
    if (entry.builtIn) setEditing({ id: null, theme: { ...entry.theme, name: `${entry.theme.name} copy`, createdBy: undefined } });
    else setEditing({ id: entry.id, theme: entry.theme });
  };

  const singleMode = () => {
    const theme = active()?.theme;
    const modes = theme ? modesOf(theme) : [];
    return modes.length === 1 ? { name: theme!.name, mode: modes[0] } : null;
  };

  return (
    <>
      <Show when={singleMode()}>
        {(only) => (
          <p class="hint settings-footnote">
            {only().name} only has a {only().mode} version, so Grimoire stays {only().mode} while it is on.
          </p>
        )}
      </Show>
      <div class="theme-grid" role="radiogroup" aria-label="Theme">
        <For each={themes()?.themes ?? []}>
          {(entry) => {
            const colors = createMemo(() => palette(entry.theme, modeFor(entry.theme, onScreen())));
            const selected = () => themes()?.active === entry.id;
            return (
              <button
                type="button"
                role="radio"
                aria-checked={selected() ? "true" : "false"}
                class={["theme-card", { selected: selected() }]}
                onClick={() => choose(entry)}
              >
                <span class="theme-thumb" style={{ background: colors().canvas, "border-color": colors().border }} aria-hidden="true">
                  <span class="theme-thumb-side" style={{ background: colors().sidebar, "border-color": colors().border }} />
                  <span class="theme-thumb-body">
                    <i style={{ background: colors().userBubble, width: "45%", "align-self": "flex-end" }} />
                    <i style={{ background: colors().text, width: "80%" }} />
                    <i style={{ background: colors().textMuted, width: "60%" }} />
                    <b style={{ background: colors().accent }} />
                  </span>
                </span>
                <span class="theme-name">{entry.theme.name}</span>
                <Show when={entry.theme.createdBy}>{(by) => <span class="theme-by">Made by {nameOf(by())}</span>}</Show>
              </button>
            );
          }}
        </For>
      </div>
      <div class="theme-actions">
        <button type="button" class="ghost small" disabled={!active()} onClick={edit}>
          {active()?.builtIn ? "Make a copy to edit" : "Edit theme"}
        </button>
        <button type="button" class="ghost small" onClick={() => api.openThemesFolder().catch((e) => setError(errorText(e)))}>
          Open themes folder
        </button>
      </div>
      <For each={themes()?.problems ?? []}>
        {(problem) => (
          <p class="error-box settings-error">
            {problem.file}: {problem.error}
          </p>
        )}
      </For>
      <Show when={error()}>
        <p class="error-box settings-error" role="alert">
          {error()}
        </p>
      </Show>
      <Show when={trying()}>
        {(entry) => (
          <Shield place="center">
            <div class="approval" role="alertdialog" aria-label={`Apply ${entry().theme.name}?`}>
              <div>
                <p class="approval-title">Previewing {entry().theme.name}</p>
                <p class="approval-detail">
                  Made by {nameOf(entry().theme.createdBy!)}. A theme can restyle everything, so apply it only if Grimoire looks right.
                </p>
              </div>
              <div class="approval-actions">
                <span class="spacer" />
                <button class="ghost small" onClick={stopTrying}>
                  Cancel
                </button>
                <button
                  class="primary small"
                  onClick={async () => {
                    await apply(entry());
                    stopTrying();
                  }}
                >
                  Apply theme
                </button>
              </div>
            </div>
          </Shield>
        )}
      </Show>
      <Show when={editing()}>
        {(target) => (
          <Sheet label="Edit theme" wide onClose={() => setEditing(null)}>
            <ThemeEditor id={target().id} theme={target().theme} startMode={modeFor(target().theme, onScreen())} onClose={() => setEditing(null)} />
          </Sheet>
        )}
      </Show>
    </>
  );
}

/** Folds shared colors into each appearance, so the editor works on one set per appearance. */
function draftFrom(theme: ThemeFile): ThemeFile {
  const variants: Partial<Record<Mode, Colors>> = {};
  for (const mode of modesOf(theme)) variants[mode] = seeds(theme, mode);
  return { name: theme.name, variants, css: theme.css ?? "" };
}

function ThemeEditor(props: { id: string | null; theme: ThemeFile; startMode: Mode; onClose: () => void }) {
  const [draft, setDraft] = createStore<ThemeFile>(draftFrom(props.theme));
  const [mode, setMode] = createSignal<Mode>(props.startMode);
  const [error, setError] = createSignal("");
  const [saving, setSaving] = createSignal(false);
  const [confirmDelete, setConfirmDelete] = createSignal(false);
  let confirmTimer: number | undefined;
  let nameField!: HTMLInputElement;
  onSettled(() => nameField.focus());

  // The whole app previews the draft in the appearance being edited, and goes back on close.
  createEffect(
    () => JSON.stringify(draft),
    (json) => previewTheme(JSON.parse(json)),
  );
  createEffect(mode, (m) => previewMode(m));
  onCleanup(() => {
    clearTimeout(confirmTimer);
    previewTheme(null);
    previewMode(null);
  });

  const has = (m: Mode) => modesOf(draft).includes(m);
  const shown = createMemo(() => (has(mode()) ? palette(draft, mode()) : null));
  const own = (role: Role) => draft.variants?.[mode()]?.[role];

  const setRole = (role: Role, value: string | undefined) =>
    setDraft((d) => {
      const colors = (d.variants![mode()] ??= {});
      if (value) colors[role] = value.toLowerCase();
      else delete colors[role];
    });

  // A new appearance starts from the other one's colors, adjusted to read well.
  const addMode = () => {
    const other = palette(draft, mode() === "dark" ? "light" : "dark");
    const colors =
      mode() === "light"
        ? { canvas: mix("#ffffff", other.accent, 0.04), accent: mix(other.accent, "#000000", 0.3) }
        : { canvas: mix("#16141c", other.accent, 0.06), accent: mix(other.accent, "#ffffff", 0.3) };
    setDraft((d) => {
      d.variants![mode()] = colors;
    });
  };

  const removeMode = () =>
    setDraft((d) => {
      delete d.variants![mode()];
    });

  const save = async (e: Event) => {
    e.preventDefault();
    setSaving(true);
    setError("");
    try {
      const saved = await api.saveTheme(props.id, JSON.parse(JSON.stringify(draft)));
      await api.setTheme(saved.id);
      props.onClose();
    } catch (err) {
      setError(errorText(err));
    } finally {
      setSaving(false);
    }
  };

  const remove = async () => {
    if (!confirmDelete()) {
      setConfirmDelete(true);
      confirmTimer = window.setTimeout(() => setConfirmDelete(false), 4000);
      return;
    }
    try {
      await api.deleteTheme(props.id!);
      props.onClose();
    } catch (err) {
      setError(errorText(err));
    }
  };

  return (
    <form class="theme-editor" onSubmit={save}>
      <h1>{props.id ? "Edit theme" : "New theme"}</h1>
      <label class="field">
        <span>Name</span>
        <input
          ref={nameField}
          value={draft.name}
          maxlength={60}
          required
          onInput={(e) =>
            setDraft((d) => {
              d.name = e.currentTarget.value;
            })
          }
        />
      </label>

      <div class="theme-editor-mode">
        <Segmented
          label="Appearance"
          value={mode()}
          options={MODES.map((m) => ({ value: m, label: m === "dark" ? "Dark" : "Light" }))}
          onChange={setMode}
        />
        <Show when={has(mode()) && modesOf(draft).length === 2}>
          <button type="button" class="ghost small" onClick={removeMode}>
            Remove {mode()} version
          </button>
        </Show>
      </div>

      <Show
        when={shown()}
        fallback={
          <div class="theme-missing">
            <p class="hint">This theme has no {mode()} version, so Grimoire stays {modesOf(draft)[0]} while it is on.</p>
            <button type="button" class="ghost small" onClick={addMode}>
              Add a {mode()} version
            </button>
          </div>
        }
      >
        {(colors) => (
          <div class="theme-roles">
            <For each={ROLES}>
              {(role) => {
                const value = () => colors()[role];
                const auto = () => !own(role);
                return (
                  <div class="theme-role">
                    <input
                      type="color"
                      class="theme-color"
                      aria-label={ROLE_LABELS[role]}
                      value={value().slice(0, 7)}
                      onInput={(e) => setRole(role, e.currentTarget.value)}
                    />
                    <span class="theme-role-label">{ROLE_LABELS[role]}</span>
                    <input
                      class="theme-hex"
                      aria-label={`${ROLE_LABELS[role]} hex`}
                      value={value()}
                      spellcheck={false}
                      onChange={(e) => {
                        const hex = e.currentTarget.value.trim();
                        if (HEX.test(hex)) setRole(role, hex);
                        else e.currentTarget.value = value();
                      }}
                    />
                    <Show
                      when={!auto() && !SEEDS.includes(role)}
                      fallback={<span class="theme-auto">{auto() ? "Auto" : ""}</span>}
                    >
                      <button type="button" class="theme-reset" title="Go back to the automatic color" onClick={() => setRole(role, undefined)}>
                        Reset
                      </button>
                    </Show>
                  </div>
                );
              }}
            </For>
          </div>
        )}
      </Show>

      <label class="field">
        <span>Custom CSS</span>
        <textarea
          class="theme-css"
          rows={5}
          spellcheck={false}
          value={draft.css ?? ""}
          placeholder={'[data-slot="sidebar"] { border-right-width: 2px; }'}
          onInput={(e) =>
            setDraft((d) => {
              d.css = e.currentTarget.value;
            })
          }
        />
      </label>
      <p class="hint">
        Applies after Grimoire's styles. Stable hooks: <code>[data-slot]</code> on the sidebar, page, chat, messages, and composer, plus the
        variables <code>--font-ui</code> and <code>--font-display</code>.
      </p>

      <Show when={error()}>
        <p class="error-box" role="alert">
          {error()}
        </p>
      </Show>
      <div class="form-actions">
        <Show when={props.id}>
          <button type="button" class="ghost danger" onClick={() => void remove()}>
            {confirmDelete() ? "Click again to delete" : "Delete theme"}
          </button>
          <ExportButton kind="theme" id={props.id!} onError={setError} />
        </Show>
        <span class="spacer" />
        <button type="button" class="ghost" onClick={() => props.onClose()}>
          Cancel
        </button>
        <button class="primary" type="submit" disabled={saving() || !draft.name.trim()}>
          Save theme
        </button>
      </div>
    </form>
  );
}
