import { createSignal, For, Show } from "solid-js";
import BotAvatar from "./BotAvatar";
import { api, errorText, onEvent, onMountAsync, type Agent, type ApprovalDecision, type ApprovalRequest } from "./api";
import "./approvals.css";

/** Familiars' requests that wait on the person, shown in the corner until answered or timed out. */
export default function Approvals(props: { agents: Agent[] }) {
  const [requests, setRequests] = createSignal<ApprovalRequest[]>([]);
  const [error, setError] = createSignal("");
  const drop = (id: string) => setRequests((list) => list.filter((r) => r.id !== id));

  onMountAsync(async (keep) => {
    keep(await onEvent<ApprovalRequest>("approval_requested", (request) => setRequests((list) => [...list.filter((r) => r.id !== request.id), request])));
    keep(await onEvent<string>("approval_closed", drop));
    // Requests sent before this window loaded.
    const waiting = await api.waitingApprovals();
    setRequests((list) => [...list, ...waiting.filter((w) => !list.some((r) => r.id === w.id))]);
  });

  const answer = async (request: ApprovalRequest, decision: ApprovalDecision) => {
    setError("");
    drop(request.id);
    try {
      await api.answerApproval(request.id, decision);
    } catch (e) {
      setError(errorText(e));
    }
  };

  return (
    <Show when={requests().length > 0 || error()}>
      <div class="approvals" role="region" aria-label="Requests from familiars">
        <For each={requests()}>
          {(request) => {
            const agent = () => props.agents.find((a) => a.id === request.agentId);
            return (
              <div class="approval" role="alertdialog" aria-label={request.title} style={agent() ? { "--agent": agent()!.avatar.color } : undefined}>
                <div class="approval-head">
                  <Show when={agent()}>{(a) => <BotAvatar avatar={a().avatar} size={28} mood="concerned" />}</Show>
                  <div>
                    <p class="approval-title">{request.title}</p>
                    <p class="approval-detail">{request.detail}</p>
                  </div>
                </div>
                <div class="approval-actions">
                  <button class="ghost small" onClick={() => void answer(request, "deny")}>
                    Deny
                  </button>
                  <span class="spacer" />
                  <Show when={request.canRemember}>
                    <button class="ghost small" onClick={() => void answer(request, "always")}>
                      Always allow
                    </button>
                  </Show>
                  <button class="primary small" onClick={() => void answer(request, "once")}>
                    Allow once
                  </button>
                </div>
              </div>
            );
          }}
        </For>
        <Show when={error()}>
          <p class="error-box" role="alert">
            {error()}
          </p>
        </Show>
      </div>
    </Show>
  );
}
