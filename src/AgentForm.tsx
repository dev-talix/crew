import { createEffect, createSignal, For, onCleanup, onSettled, Show } from "solid-js";
import AvatarStudio from "./AvatarStudio";
import { Segmented } from "./ui";
import { inkFor, randomAvatar, type Avatar } from "./avatar";
import { api, createFetch, errorText, onEvent, onMountAsync, PROVIDER_NAMES, shortPath, type Access, type Agent, type Coven, type HostCheck, type Peer, type Provider } from "./api";
import { runsOnOptions } from "./linked";
import { effortName, modelCatalog, modelName } from "./models";
import { BudgetField } from "./BudgetControls";
import { formatTokens, parseTokens, type BudgetPeriod } from "./budgets";
import { ExportButton } from "./ImportExport";
import TemplateImport from "./TemplateImport";
import { followUntouched } from "./panel";

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

function checkSummary(result: HostCheck | string, provider: Provider): string {
  if (typeof result === "string") return result;
  if (!result.reachable) return result.error ?? "Could not connect.";
  const where = (path: string | null) => path ?? "not found";
  const missing = result[provider] ? "" : ` Install ${PROVIDER_NAMES[provider]} on that Mac before you summon this familiar.`;
  return `Connected. Codex: ${where(result.codex)}. Claude: ${where(result.claude)}.${missing}`;
}

export default function AgentForm(props: {
  agent?: Agent;
  covens?: Coven[];
  onSaved: (agent: Agent) => void;
  onCancel?: () => void;
  onDeleted?: (id: string) => void;
  /** Offers importing a marketplace template instead; gets the new familiar's id. */
  onImported?: (agentId: string) => void;
}) {
  const editing = !!props.agent;
  const [name, setName] = createSignal(props.agent?.name ?? "");
  const [provider, setProvider] = createSignal<Provider>(props.agent?.provider ?? "codex");
  const [model, setModel] = createSignal(props.agent?.model ?? "");
  const [effort, setEffort] = createSignal(props.agent?.effort ?? "");
  const [coven, setCoven] = createSignal(props.agent?.covenId ?? "");
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
  // "here", "ssh", or "peer:<id>" for a linked Mac.
  const [runsOn, setRunsOn] = createSignal("here");
  const remote = () => runsOn() === "ssh";
  const [peers, setPeers] = createSignal<Peer[]>([]);
  const peer = () => (runsOn().startsWith("peer:") ? peers().find((p) => `peer:${p.id}` === runsOn()) : undefined);
  const linkedMac = () => runsOn().startsWith("peer:");
  onMountAsync(async (keep) => {
    if (editing) return;
    const load = () => api.linkStatus().then((s) => setPeers(s.peers), () => setPeers([]));
    keep(await onEvent("links_changed", () => void load()));
    await load();
  });
  const [host, setHost] = createSignal(props.agent?.host ?? "");
  const [checking, setChecking] = createSignal(false);
  const [check, setCheck] = createSignal<HostCheck | string | null>(null);
  // A familiar saved while its Mac was offline, shown here before the form closes.
  const [savedPending, setSavedPending] = createSignal<Agent | null>(null);
  const checkConnection = async () => {
    setChecking(true);
    setCheck(null);
    try {
      // Reaching the Mac also finishes the setup of familiars waiting on it.
      setCheck(await api.checkHost(host()));
    } catch (err) {
      setCheck(errorText(err));
    } finally {
      setChecking(false);
    }
  };
  const [access, setAccess] = createSignal<Access>(props.agent?.access ?? "workspace");
  const [avatar, setAvatar] = createSignal<Avatar>(props.agent?.avatar ?? randomAvatar());
  const [error, setError] = createSignal("");
  const [saving, setSaving] = createSignal(false);
  const [confirmDelete, setConfirmDelete] = createSignal(false);
  const [importing, setImporting] = createSignal(false);
  const [budget, setBudget] = createSignal("");
  // Settings sit beside the chat, which can change the model, effort, and access while they're open.
  // A field the user hasn't touched follows those changes, so saving doesn't undo them.
  if (props.agent) {
    let before = props.agent;
    createEffect(
      () => props.agent!,
      (after) => {
        setName((v) => followUntouched(v, before.name, after.name));
        setModel((v) => followUntouched(v, before.model ?? "", after.model ?? ""));
        setEffort((v) => followUntouched(v, before.effort ?? "", after.effort ?? ""));
        setCoven((v) => followUntouched(v, before.covenId ?? "", after.covenId ?? ""));
        setAccess((v) => followUntouched(v, before.access, after.access));
        setAvatar((v) => followUntouched(v, before.avatar, after.avatar));
        before = after;
      },
    );
  }
  const [period, setPeriod] = createSignal<BudgetPeriod>("day");
  let hadBudget = false;
  onSettled(() => {
    const id = props.agent?.id;
    if (!id) return;
    api.listBudgets().then((list) => {
      const saved = list.find((b) => b.agentId === id);
      hadBudget = !!saved;
      if (saved) {
        setBudget(formatTokens(saved.tokens));
        setPeriod(saved.period);
      }
    }, () => undefined);
  });
  let confirmTimer: number | undefined;
  onCleanup(() => clearTimeout(confirmTimer));

  const submit = async (e: SubmitEvent) => {
    e.preventDefault();
    setError("");
    const tokens = parseTokens(budget());
    if (Number.isNaN(tokens)) {
      setError("Write the budget as a number of tokens, like 50000 or 50k, or leave it empty for no limit.");
      return;
    }
    if (!editing && remote() && !host().trim()) {
      setError("Write the other Mac's SSH destination, like me@studio.local.");
      return;
    }
    const target = peer();
    if (!editing && linkedMac() && !target?.online) {
      setError(`${target?.name ?? "That Mac"} is offline. Pick another Mac, or try again when it's back.`);
      return;
    }
    setSaving(true);
    try {
      const saved = props.agent
        ? await api.updateAgent(props.agent.id, {
            name: name(),
            model: model(),
            access: access(),
            avatar: avatar(),
            // The chat beside these settings can switch to a model that doesn't offer the effort picked here.
            effort: efforts().includes(effort()) ? effort() : "",
            covenId: coven() || null,
          })
        : await api.createAgent(
            target
              ? { name: name(), provider: provider(), model: model(), workspace: "", access: access(), avatar: avatar(), effort: effort(), peerId: target.id }
              : {
                  name: name(),
                  provider: provider(),
                  model: model(),
                  workspace: workspace(),
                  access: access(),
                  avatar: avatar(),
                  effort: effort(),
                  covenId: coven() || null,
                  host: remote() ? host() : "",
                },
          );
      // A linked Mac keeps its familiars' budgets; this Mac can't set one there.
      if (!target && (tokens !== null || hadBudget)) await api.setBudget(saved.id, tokens === null ? null : { tokens, period: period() });
      if (!editing && saved.setupPending) {
        setSavedPending(saved);
        return;
      }
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
    <>
      <form
        class={["agent-form", { editing }]}
        style={{ "--agent": avatar().color, "--agent-ink": inkFor(avatar().color) }}
        onSubmit={submit}
      >
        <h1>{editing ? "Familiar settings" : "Summon a familiar"}</h1>
        <Show when={!editing && props.onImported}>
          <button type="button" class="ghost small template-entry" onClick={() => setImporting(true)}>
            Import a Grok Bot template
          </button>
        </Show>
        <AvatarStudio avatar={avatar()} name={name()} onChange={setAvatar} />

        <div class="form-grid">
          <label class="field">
            <span>Name</span>
            <input value={name()} onInput={(e) => setName(e.currentTarget.value)} placeholder="Site builder" autofocus required maxlength={60} />
          </label>

          <Show
            when={!editing}
            fallback={
              <>
                <div class="field">
                  <span>Runs on</span>
                  <p class="readonly">
                    {PROVIDER_NAMES[provider()]}
                    {props.agent!.host ? ` on ${props.agent!.host}` : ""} in {shortPath(props.agent!.workspace)}
                  </p>
                </div>
                <Show when={props.agent!.host}>
                  <div class="field">
                    <span>Connection</span>
                    <button type="button" class="ghost check-button" disabled={checking()} onClick={checkConnection}>
                      {checking() ? "Checking..." : "Check connection"}
                    </button>
                  </div>
                  <Show when={check()}>
                    {(result) => (
                      <p class="hint check-result" role="status">
                        {checkSummary(result(), provider())}
                      </p>
                    )}
                  </Show>
                </Show>
              </>
            }
          >
            <div class="field">
              <span>Provider</span>
              <Segmented
                label="Provider"
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

          <Show when={props.covens?.length && !linkedMac()}>
            <label class="field">
              <span>Coven</span>
              <select value={coven()} onChange={(e) => setCoven(e.currentTarget.value)}>
                <option value="">Unbound</option>
                <For each={props.covens}>{(c) => <option value={c.id}>{c.name}</option>}</For>
              </select>
            </label>
          </Show>

          <Show when={!editing}>
            <div class="field wide">
              <span>Runs on</span>
              <Segmented label="Runs on" value={runsOn()} options={runsOnOptions(peers())} onChange={setRunsOn} />
              <Show when={peer()}>
                {(mac) => (
                  <p class="hint">
                    It lives on {mac().name} and shows up here too. Only {mac().name} can change or delete it, and{" "}
                    {PROVIDER_NAMES[provider()]} must be installed and signed in there.
                  </p>
                )}
              </Show>
              <Show when={remote()}>
                <p class="hint">
                  SSH must sign in with a key, without a password prompt, and {PROVIDER_NAMES[provider()]} must be installed and signed in on
                  that Mac. Familiars there run without Grimoire's tools and can't take attachments yet.
                </p>
              </Show>
            </div>
            <Show when={remote()}>
              <label class="field">
                <span>SSH destination</span>
                <input
                  value={host()}
                  onInput={(e) => {
                    setHost(e.currentTarget.value);
                    setCheck(null);
                  }}
                  placeholder="me@studio.local"
                  autocapitalize="off"
                  autocorrect="off"
                  spellcheck={false}
                  required
                />
              </label>
              <div class="field">
                <span>Connection</span>
                <button type="button" class="ghost check-button" disabled={checking() || !host().trim()} onClick={checkConnection}>
                  {checking() ? "Checking..." : "Check connection"}
                </button>
              </div>
              <Show when={check()}>
                {(result) => (
                  <p class="hint check-result" role="status">
                    {checkSummary(result(), provider())}
                  </p>
                )}
              </Show>
            </Show>
            <Show when={!linkedMac()}>
              <label class="field">
                <span>{remote() ? "Workspace on that Mac" : "Workspace folder"}</span>
                <input
                  value={workspace()}
                  onInput={(e) => setWorkspace(e.currentTarget.value)}
                  placeholder={remote() ? "A new folder in ~/Grimoire there" : "A new folder in ~/Grimoire"}
                  autocapitalize="off"
                  autocorrect="off"
                  spellcheck={false}
                />
              </label>
            </Show>
          </Show>

          <Show when={!linkedMac()}>
            <BudgetField text={budget()} period={period()} onText={setBudget} onPeriod={setPeriod} />
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

        <Show when={savedPending()}>
          {(saved) => (
            <p class="hint" role="status">
              {saved().name} is saved, but {saved().host} didn't answer, so its setup is pending. Grimoire finishes it when that Mac
              connects. Until then, chats and rituals with {saved().name} say the Mac is offline.
            </p>
          )}
        </Show>

        <div class="form-actions">
          <Show when={editing}>
            <button type="button" class="ghost danger" onClick={remove}>
              {confirmDelete() ? "Click again to delete" : "Delete familiar"}
            </button>
            <ExportButton kind="familiar" id={props.agent!.id} onError={setError} />
          </Show>
          <span class="spacer" />
          <Show when={props.onCancel}>
            <button type="button" class="ghost" onClick={() => props.onCancel?.()}>
              Cancel
            </button>
          </Show>
          <Show
            when={savedPending()}
            fallback={
              <button class="primary" type="submit" disabled={saving() || !name().trim()}>
                {editing ? "Save changes" : "Summon familiar"}
              </button>
            }
          >
            {(saved) => (
              <button type="button" class="primary" onClick={() => props.onSaved(saved())}>
                Open {saved().name}
              </button>
            )}
          </Show>
        </div>
      </form>
      {/* Outside the form: the sheet's own forms would otherwise submit the Summon form too. */}
      <Show when={importing() && props.onImported}>
        {(onImported) => <TemplateImport onClose={() => setImporting(false)} onCreated={(id) => (setImporting(false), onImported()(id))} />}
      </Show>
    </>
  );
}
