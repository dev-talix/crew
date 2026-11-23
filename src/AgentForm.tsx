import { createSignal, For, onCleanup, Show } from "solid-js";
import AvatarStudio from "./AvatarStudio";
import { Segmented } from "./ui";
import { inkFor, randomAvatar, type Avatar } from "./avatar";
import { api, createFetch, errorText, PROVIDER_NAMES, shortPath, type Access, type Agent, type Provider } from "./api";
import { effortName, modelCatalog, modelName } from "./models";

const ACCESS_HELP: Record<Provider, Record<Access, string>> = {
  codex: {
    workspace: "Runs in the Codex sandbox and changes files only in its workspace.",
    full: "Runs without a sandbox. It can change any file and use the network.",
  },
  claude: {
    workspace: "Edits files in its workspace. Shell commands are refused.",
    full: "Runs any command without asking.",
  },
};

export default function AgentForm(props: {
  agent?: Agent;
  onSaved: (agent: Agent) => void;
  onCancel?: () => void;
  onDeleted?: (id: string) => void;
}) {
  const editing = !!props.agent;
  const [name, setName] = createSignal(props.agent?.name ?? "");
  const [provider, setProvider] = createSignal<Provider>(props.agent?.provider ?? "codex");
  const [model, setModel] = createSignal(props.agent?.model ?? "");
  const [effort, setEffort] = createSignal(props.agent?.effort ?? "");
  const catalog = createFetch(provider, modelCatalog);
  const models = () => catalog() ?? [];
  const efforts = () =>
    models().find((m) => m.id === model())?.efforts ??
    models().find((m) => m.isDefault)?.efforts ?? ["low", "medium", "high", "xhigh", "max"];
  const defaultLabel = () => {
    const fallback = props.agent?.activeModel ?? models().find((m) => m.isDefault)?.id;
    return fallback ? `Default (${modelName(fallback, models())})` : "Default";
  };
  const chooseProvider = (next: Provider) => {
    setProvider(next);
    setModel("");
    setEffort("");
  };
  const [workspace, setWorkspace] = createSignal("");
  const [access, setAccess] = createSignal<Access>(props.agent?.access ?? "workspace");
  const [avatar, setAvatar] = createSignal<Avatar>(props.agent?.avatar ?? randomAvatar());
  const [error, setError] = createSignal("");
  const [saving, setSaving] = createSignal(false);
  const [confirmDelete, setConfirmDelete] = createSignal(false);
  let confirmTimer: number | undefined;
  onCleanup(() => clearTimeout(confirmTimer));

  const submit = async (e: SubmitEvent) => {
    e.preventDefault();
    setError("");
    setSaving(true);
    try {
      const saved = props.agent
        ? await api.updateAgent(props.agent.id, { name: name(), model: model(), access: access(), avatar: avatar(), effort: effort() })
        : await api.createAgent({
            name: name(),
            provider: provider(),
            model: model(),
            workspace: workspace(),
            access: access(),
            avatar: avatar(),
            effort: effort(),
          });
      props.onSaved(saved);
    } catch (err) {
      setError(errorText(err));
    } finally {
      setSaving(false);
    }
  };

  const remove = async () => {
    const agent = props.agent;
    if (!agent) return;
    if (!confirmDelete()) {
      setConfirmDelete(true);
      confirmTimer = window.setTimeout(() => setConfirmDelete(false), 4000);
      return;
    }
    try {
      await api.deleteAgent(agent.id);
      props.onDeleted?.(agent.id);
    } catch (err) {
      setError(errorText(err));
    }
  };

  return (
    <form
      class={["agent-form", { editing }]}
      style={{ "--agent": avatar().color, "--agent-ink": inkFor(avatar().color) }}
      onSubmit={submit}
    >
      <h1>{editing ? "Familiar settings" : "Summon a familiar"}</h1>
      <AvatarStudio avatar={avatar()} name={name()} onChange={setAvatar} />

      <div class="form-grid">
        <label class="field">
          <span>Name</span>
          <input value={name()} onInput={(e) => setName(e.currentTarget.value)} placeholder="Site builder" autofocus required maxlength={60} />
        </label>

        <Show
          when={!editing}
          fallback={
            <div class="field">
              <span>Runs on</span>
              <p class="readonly">
                {PROVIDER_NAMES[provider()]} in {shortPath(props.agent!.workspace)}
              </p>
            </div>
          }
        >
          <div class="field">
            <span>Runs on</span>
            <Segmented
              label="Runs on"
              value={provider()}
              options={[
                { value: "codex", label: "Codex" },
                { value: "claude", label: "Claude" },
              ]}
              onChange={chooseProvider}
            />
          </div>
        </Show>

        <label class="field">
          <span>Model</span>
          <select value={model()} onChange={(e) => setModel(e.currentTarget.value)}>
            <option value="">{defaultLabel()}</option>
            <For each={models()}>{(option) => <option value={option.id}>{option.name}</option>}</For>
            <Show when={model() && !models().some((m) => m.id === model())}>
              <option value={model()}>{modelName(model())}</option>
            </Show>
          </select>
        </label>

        <label class="field">
          <span>Effort</span>
          <select value={effort()} onChange={(e) => setEffort(e.currentTarget.value)}>
            <option value="">Default</option>
            <For each={efforts()}>{(level) => <option value={level}>{effortName(level)}</option>}</For>
          </select>
        </label>

        <Show when={!editing}>
          <label class="field">
            <span>Workspace folder</span>
            <input value={workspace()} onInput={(e) => setWorkspace(e.currentTarget.value)} placeholder="A new folder in ~/Grimoire" />
          </label>
        </Show>

        <div class="field wide">
          <span>Access</span>
          <Segmented
            label="Access"
            value={access()}
            options={[
              { value: "workspace", label: "Workspace only" },
              { value: "full", label: "Full access" },
            ]}
            onChange={setAccess}
          />
          <p class="hint">{ACCESS_HELP[provider()][access()]}</p>
        </div>
      </div>

      <Show when={error()}>
        <p class="error-box" role="alert">
          {error()}
        </p>
      </Show>

      <div class="form-actions">
        <Show when={editing}>
          <button type="button" class="ghost danger" onClick={remove}>
            {confirmDelete() ? "Click again to delete" : "Delete familiar"}
          </button>
        </Show>
        <span class="spacer" />
        <Show when={props.onCancel}>
          <button type="button" class="ghost" onClick={() => props.onCancel?.()}>
            Cancel
          </button>
        </Show>
        <button class="primary" type="submit" disabled={saving() || !name().trim()}>
          {editing ? "Save changes" : "Summon familiar"}
        </button>
      </div>
    </form>
  );
}
