import { createMemo, createSignal, For, onCleanup, Show } from "solid-js";
import { api, errorText, onEvent, onMountAsync, type Agent } from "./api";
import { enabledLabel, sourceLabel, type AddFrom, type Skill, type SkillFile, type SkillPreview } from "./skills";
import { Page, Segmented, Sheet } from "./ui";
import "./skills.css";

/** Every file of a skill, each with its full text. */
function FileList(props: { files: SkillFile[] }) {
  return (
    <div class="skill-files">
      <For each={props.files}>
        {(file) => (
          <section class="skill-file">
            <h2 class="skill-file-path">{file.path}</h2>
            <pre>{file.text}</pre>
          </section>
        )}
      </For>
    </div>
  );
}

export default function SkillsPage(props: { agents: Agent[] }) {
  const [skills, setSkills] = createSignal<Skill[]>([]);
  const [error, setError] = createSignal("");
  const [open, setOpen] = createSignal<{ skill: Skill; files: SkillFile[] } | null>(null);
  const [preview, setPreview] = createSignal<SkillPreview | null>(null);
  const [confirmDelete, setConfirmDelete] = createSignal<string | null>(null);
  const [mode, setMode] = createSignal<"text" | "folder" | "url">("text");
  const [text, setText] = createSignal("");
  const [url, setUrl] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  let confirmTimer: number | undefined;
  onCleanup(() => clearTimeout(confirmTimer));

  const load = () => api.skillLibrary().then(setSkills, (e) => setError(errorText(e)));
  onMountAsync(async (keep) => {
    keep(await onEvent("skills_changed", () => void load()));
    keep(await onEvent("agents_changed", () => void load()));
    await load();
  });
  const names = createMemo(() => Object.fromEntries(props.agents.map((a) => [a.id, a.name])));

  const run = async (action: () => Promise<unknown>) => {
    setError("");
    setBusy(true);
    try {
      await action();
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(false);
    }
  };

  const view = (skill: Skill) => run(async () => setOpen({ skill, files: await api.skillFiles(skill.name) }));
  const startPreview = (from: AddFrom) => run(async () => setPreview(await api.previewSkill(from)));
  const save = (shown: SkillPreview) =>
    run(async () => {
      await api.addSkill(shown);
      setPreview(null);
      setText("");
      setUrl("");
      await load();
    });
  const remove = (skill: Skill) => {
    if (confirmDelete() !== skill.name) {
      setConfirmDelete(skill.name);
      clearTimeout(confirmTimer);
      confirmTimer = window.setTimeout(() => setConfirmDelete(null), 4000);
      return;
    }
    setConfirmDelete(null);
    setOpen(null);
    void run(async () => {
      await api.deleteSkill(skill.name);
      await load();
    });
  };

  return (
    <Page title="Skills">
      <div class="settings skills">
        <p class="settings-detail">
          Skills are instructions for one kind of task, in the SKILL.md format Claude Code and Codex read on their own. Turn a skill on for a
          familiar in that familiar's settings. Familiars can save skills too, after they solve something they'd do again.
        </p>
        <Show when={error()}>
          <p class="error-box settings-error" role="alert">
            {error()}
          </p>
        </Show>

        <Show when={skills().length > 0} fallback={<p class="hint">No skills yet. Add one below, or ask a familiar to save one.</p>}>
          <h2 class="section-title">Library</h2>
          <div class="list">
            <For each={skills()}>
              {(skill) => (
                <div class="list-row skill-row">
                  <button type="button" class="list-main skill-open" onClick={() => void view(skill)}>
                    <span class="list-title">{skill.name}</span>
                    <span class="list-sub skill-description">{skill.description}</span>
                    <span class="list-sub">
                      {sourceLabel(skill, names())}. {enabledLabel(skill, names())}.
                      {skill.files.length > 1 ? ` ${skill.files.length} files.` : ""}
                    </span>
                  </button>
                  <div class="list-actions">
                    <button type="button" class="ghost small" onClick={() => void view(skill)}>
                      View
                    </button>
                    <button type="button" class="ghost small danger" onClick={() => remove(skill)}>
                      {confirmDelete() === skill.name ? "Click again" : "Delete"}
                    </button>
                  </div>
                </div>
              )}
            </For>
          </div>
        </Show>

        <h2 class="section-title">Add a skill</h2>
        <div class="skill-add">
          <Segmented
            label="Add from"
            value={mode()}
            options={[
              { value: "text", label: "Paste SKILL.md" },
              { value: "folder", label: "Folder" },
              { value: "url", label: "GitHub link" },
            ]}
            onChange={setMode}
          />
          <Show when={mode() === "text"}>
            <form
              class="skill-add-form"
              onSubmit={(e) => {
                e.preventDefault();
                void startPreview({ kind: "text", text: text() });
              }}
            >
              <textarea
                rows={8}
                value={text()}
                onInput={(e) => setText(e.currentTarget.value)}
                aria-label="SKILL.md text"
                placeholder={"---\nname: weekly-report\ndescription: Writes the weekly report. Use when asked for it.\n---\n\n1. Collect the week's merged pull requests.\n2. ..."}
                spellcheck={false}
              />
              <button type="submit" class="ghost" disabled={busy() || !text().trim()}>
                Preview
              </button>
            </form>
          </Show>
          <Show when={mode() === "folder"}>
            <div class="skill-add-form">
              <p class="hint">Pick a folder with a SKILL.md at its top. Grimoire copies its text files and leaves out hidden files, links, and anything that isn't text.</p>
              <button type="button" class="ghost" disabled={busy()} onClick={() => void startPreview({ kind: "folder" })}>
                Choose folder…
              </button>
            </div>
          </Show>
          <Show when={mode() === "url"}>
            <form
              class="skill-add-form"
              onSubmit={(e) => {
                e.preventDefault();
                void startPreview({ kind: "url", url: url() });
              }}
            >
              <label class="field">
                <span>GitHub link to a SKILL.md or a skill's folder</span>
                <input
                  value={url()}
                  onInput={(e) => setUrl(e.currentTarget.value)}
                  placeholder="https://github.com/owner/repo/tree/main/skills/pdf"
                  autocapitalize="off"
                  autocorrect="off"
                  spellcheck={false}
                />
              </label>
              <button type="submit" class="ghost" disabled={busy() || !url().trim()}>
                {busy() ? "Reading…" : "Preview"}
              </button>
            </form>
          </Show>
        </div>
        <p class="hint settings-footnote">
          New skills start turned off for every familiar. A skill holds up to 50 text files and 1 MB. Familiars on another Mac don't get skills yet.
        </p>
      </div>

      <Show when={open()}>
        {(shown) => (
          <Sheet label={`Skill ${shown().skill.name}`} wide onClose={() => setOpen(null)}>
            <div class="skill-sheet">
              <h1>{shown().skill.name}</h1>
              <p class="hint">
                {sourceLabel(shown().skill, names())}. {enabledLabel(shown().skill, names())}.
              </p>
              <FileList files={shown().files} />
              <div class="form-actions">
                <button type="button" class="ghost danger" onClick={() => remove(shown().skill)}>
                  {confirmDelete() === shown().skill.name ? "Click again to delete" : "Delete skill"}
                </button>
                <span class="spacer" />
                <button type="button" class="primary" onClick={() => setOpen(null)}>
                  Done
                </button>
              </div>
            </div>
          </Sheet>
        )}
      </Show>

      <Show when={preview()}>
        {(shown) => (
          <Sheet label={`Add ${shown().name}`} wide onClose={() => setPreview(null)}>
            <div class="skill-sheet">
              <h1>Add {shown().name}</h1>
              <p class="hint">
                {shown().source === "you"
                  ? "Check the text before you save it."
                  : `Someone else wrote this${shown().origin ? `, at ${shown().origin}` : ""}. Read every file before you save it: a familiar with this skill on follows what it says.`}{" "}
                It comes in turned off for every familiar.
              </p>
              <p class="skill-preview-description">{shown().description}</p>
              <Show when={shown().notes.length}>
                <ul class="skill-notes">
                  <For each={shown().notes}>{(note) => <li>{note}</li>}</For>
                </ul>
              </Show>
              <FileList files={shown().files} />
              <Show when={error()}>
                <p class="error-box" role="alert">
                  {error()}
                </p>
              </Show>
              <div class="form-actions">
                <span class="spacer" />
                <button type="button" class="ghost" onClick={() => setPreview(null)}>
                  Cancel
                </button>
                <button type="button" class="primary" disabled={busy()} onClick={() => void save(shown())}>
                  Save skill
                </button>
              </div>
            </div>
          </Sheet>
        )}
      </Show>
    </Page>
  );
}
