# Crew

A small desktop app for persistent AI agents. Each agent has a name, a workspace folder, and runs on
Codex or Claude. Built with Tauri 2, Rust, SolidJS 2, and SQLite.

## Run

```sh
bun install
bunx tauri dev                    # development
bunx tauri build --bundles app    # release build: src-tauri/target/release/bundle/macos/Crew.app
```

Needs `codex` and/or `claude` installed and signed in. Crew reads `PATH` from your login shell, so
it finds them when you open it from Finder too.

## Agents and avatars

Each agent has an outline avatar you build in the avatar studio: one of six shapes (orbit, spark,
cloud, drop, ears, classic), three eye styles, and any color. Avatars blink, glance, wink, and fidget
when idle, scan while working, smile when a turn finishes, and look worried after an error. Each shape
has its own moves, like the orbit's moon or the cloud's drizzle. Motion stops when macOS Reduce Motion
is on. The avatar's color tints that agent's chat. Open an agent's settings (the gear in the chat
header) to change its avatar, name, model, effort, or access, or to delete it.

The model chip in the composer shows the model the agent is running and lets you switch it.

## Home, routines, and workflows

- **Home** lists every agent with its latest message and the next scheduled routines.
- **Routines** send a prompt to an agent on a schedule: every few hours, daily at a time, or on chosen
  weekdays. One timer sleeps until the next due run; a run missed while Crew was closed fires once
  at the next launch. Messages from a routine are labeled with its name.
- **Workflows** chain agents. Each step sends instructions to one agent and waits for its reply;
  `{{input}}` is the starting text and `{{previous}}` is the last reply (appended automatically
  when a step does not use it).
- **Agents make workflows too.** Every agent gets three Crew tools: `list_agents`, `list_workflows`, and
  `create_workflow`. Ask one to "set up a workflow that…" and it saves it; the Workflows page shows
  which agent made it. Agents cannot run, edit, or delete workflows.
- **Voice**: the microphone button in the composer dictates with macOS speech recognition.

## Settings

- **Accounts** shows whether `codex` and `claude` are installed and signed in.
- **Claude billing**: with "Claude subscription" (default), Crew removes `ANTHROPIC_API_KEY`,
  `ANTHROPIC_AUTH_TOKEN`, and `ANTHROPIC_BASE_URL` from the `claude` process so it uses your
  claude.ai Pro or Max sign-in. "API key" leaves them in place.
- **Theme**: system, light, or dark.
- **Dock**: pin your agents to the left or right edge of the screen. Click one to chat in a
  panel without opening the main window.

Shortcuts: ⌘N new agent, ⌘, settings, ⌘1 to ⌘9 open an agent.

## How it works

- **Processes.** Nothing starts at launch. Codex agents share one `codex app-server` process (JSON-RPC
  over stdio), started on the first Codex message and stopped after 10 idle minutes. Each Claude agent
  gets one `claude -p --input-format stream-json` process, started on its first message and stopped
  after 5 idle minutes. Closing Crew stops them all.
- **Sessions.** The Codex thread ID or Claude session ID is saved per agent, so a restart resumes the
  same conversation. "New chat" drops it.
- **Access.** Workspace only: Codex runs in its `workspace-write` sandbox; Claude runs in
  `acceptEdits` mode, so it can edit files but shell commands are refused. Full access: Codex
  `danger-full-access`, Claude `bypassPermissions`. Neither asks for approval during a turn.
- **Streaming.** Text goes to the window at most every 50 ms and to SQLite every 750 ms and at the
  end of each turn. Markdown renders only once a message is finished.
- **Queue.** A message sent while the agent works waits and runs next. Stop interrupts the turn
  (Claude through its interrupt request, Codex through `turn/interrupt`) and cancels the queue.
- **Agent tools.** Crew passes each agent an MCP server named `crew` (`--mcp-config` for Claude, thread
  config for Codex). The CLI starts `crew mcp <socket> <agent-id>`, which relays tool calls to the app
  over `crew.sock` in Crew's data folder (owner-only). The tools work in both access modes because they
  only touch Crew's own data.
- **Storage.** `~/Library/Application Support/dev.talix.crew/crew.db`. Default workspaces are under
  `~/Crew`. Deleting an agent keeps its workspace folder.

## Checks

```sh
bun run test                     # timeline, models, chat store, moods
cd src-tauri
cargo test                       # unit tests
cargo test -- --ignored          # live turns against the real codex and claude CLIs (costs tokens)
cargo clippy --all-targets -- -D warnings
```

## License

[MIT](LICENSE)
