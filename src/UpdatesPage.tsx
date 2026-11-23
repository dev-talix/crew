import { openUrl } from "@tauri-apps/plugin-opener";
import { createSignal, Show } from "solid-js";
import { errorText, relativeTime } from "./api";
import { RefreshIcon } from "./icons";
import { Page, Toggle } from "./ui";
import { autoCheck, checkForUpdates, setAutoCheck, updateCheck } from "./updateStore";

export default function UpdatesPage(props: { version: string }) {
  const [checking, setChecking] = createSignal(false);
  const [error, setError] = createSignal("");
  const [auto, setAuto] = createSignal(autoCheck());

  const check = async () => {
    setChecking(true);
    setError("");
    try {
      await checkForUpdates();
    } catch (e) {
      setError(errorText(e));
    } finally {
      setChecking(false);
    }
  };

  const status = () => {
    const result = updateCheck();
    if (!result) return "Not checked yet.";
    if (result.newer && result.latest) return `Version ${result.latest.version} is available.`;
    if (!result.latest) return "No releases are published yet.";
    return "You're up to date.";
  };

  return (
    <Page title="Updates">
      <div class="settings">
        <div class="list">
          <div class="list-row settings-row">
            <div class="list-main">
              <div class="list-title">Check automatically</div>
              <p class="settings-detail">Look for a new version once a day when Grimoire opens.</p>
            </div>
            <Toggle
              label="Check for updates automatically"
              checked={auto()}
              onChange={(on) => {
                setAuto(on);
                setAutoCheck(on);
              }}
            />
          </div>
          <div class="list-row settings-row">
            <div class="list-main">
              <div class="list-title">Version {props.version}</div>
              <p class="settings-detail">
                {status()}
                <Show when={updateCheck()?.latest?.publishedAt}>
                  {(at) => <> Released {relativeTime(Date.parse(at()))}.</>}
                </Show>
              </p>
            </div>
            <Show
              when={updateCheck()?.newer && updateCheck()?.latest}
              fallback={
                <button type="button" class={["ghost", "small", { spinning: checking() }]} disabled={checking()} onClick={() => void check()}>
                  <RefreshIcon /> Check for updates
                </button>
              }
            >
              {(latest) => (
                <button type="button" class="primary" onClick={() => openUrl(latest().url).catch((e) => setError(errorText(e)))}>
                  Download {latest().version}
                </button>
              )}
            </Show>
          </div>
        </div>
        <p class="hint settings-footnote">
          Grimoire checks its GitHub releases. Installing is a download for now: automatic installs need signed release builds.
        </p>
        <Show when={error()}>
          <p class="error-box settings-error" role="alert">
            {error()}
          </p>
        </Show>
      </div>
    </Page>
  );
}
