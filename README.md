# Grimoire

A small desktop app for persistent AI familiars. Each familiar has a name, a workspace folder, and
runs on Codex or Claude. Built with Tauri 2, Rust, SolidJS 2, and SQLite.

The code calls a familiar an agent, a spell a workflow, and a ritual a routine.

## Run

```sh
bun install
bunx tauri dev                    # development
bunx tauri build --bundles app    # release build: src-tauri/target/release/bundle/macos/Grimoire.app
```

Needs `codex` and/or `claude` installed and signed in. Grimoire reads `PATH` from your login shell, so
it finds them when you open it from Finder too.

## Familiars and avatars

Each familiar has an outline avatar you build in the avatar studio: one of six shapes (orbit, spark,
cloud, drop, ears, classic), three eye styles, and any color. Avatars blink, glance, wink, and fidget
when idle, scan while working, smile when a turn finishes, and look worried after an error. Each shape
has its own moves, like the orbit's moon or the cloud's drizzle. Motion stops when macOS Reduce Motion
is on. The avatar's color tints that familiar's chat. Open a familiar's settings (the gear in the chat
header) to change its avatar, name, model, effort, or access, or to delete it.

The model chip in the composer shows the model the familiar is running and lets you switch it.

## Home, rituals, and spells

- **Home** lists every familiar with its latest message and the next scheduled rituals.
- **Rituals** send a prompt to a familiar on a schedule: every few hours, daily at a time, or on chosen
  weekdays. One timer sleeps until the next due run; a run missed while Grimoire was closed fires once
  at the next launch. Messages from a ritual are labeled with its name.
- **Spells** chain familiars. Each step sends instructions to one familiar and waits for its reply;
  `{{input}}` is the starting text and `{{previous}}` is the last reply (appended automatically
  when a step does not use it). The sidebar lists them; click one to cast it.
- **Familiars make spells and themes too.** Every familiar gets Grimoire tools: `list_familiars`,
  `list_spells`, `create_spell`, `list_themes`, `create_theme`, and `set_theme`. Ask one to "make a
  spell that…" or "make a theme that feels like a rainy library" and it saves it; Grimoire shows
  which familiar made it. Familiars cannot cast, edit, or delete spells.
- **Voice**: the microphone button in the composer dictates with macOS speech recognition.

## Settings

- **Accounts** shows whether `codex` and `claude` are installed and signed in.
- **Claude billing**: with "Claude subscription" (default), Grimoire removes `ANTHROPIC_API_KEY`,
  `ANTHROPIC_AUTH_TOKEN`, and `ANTHROPIC_BASE_URL` from the `claude` process so it uses your
  claude.ai Pro or Max sign-in. "API key" leaves them in place.
- **Dock**: pin your familiars to the left or right edge of the screen. Click one to chat in a
  panel without opening the main window.
- **Appearance**: the mode (system, light, or dark) and the theme. See [Themes](#themes).

Shortcuts: ⌘N summon a familiar, ⌘, settings, ⌘1 to ⌘9 open a familiar.

## Themes

Grimoire ships seven themes named for old inks and pigments: Tyrian (the default), Iron gall,
Verdigris, Dragon's blood, Orpiment, Lapis, and Moonstone. Each has a dark and a light version.

Your own themes are JSON files in `~/Library/Application Support/dev.talix.grimoire/themes`. Settings
opens that folder, and Grimoire reloads it whenever its window comes back to the front, so you can edit
a theme in any text editor. "Make a copy to edit" in Settings opens the same file in an editor with
color pickers and a live preview.

```json
{
  "version": 1,
  "name": "Rainy library",
  "colors": { "accent": "#8fb3c9" },
  "variants": {
    "dark": { "canvas": "#14181c" },
    "light": { "canvas": "#f3f5f6", "accent": "#2f6d8f" }
  },
  "css": "[data-slot=\"sidebar\"] { border-right-width: 2px; }"
}
```

- `colors` applies to both appearances and `variants.dark` or `variants.light` override it. Each
  appearance needs `canvas` (the background) and `accent`; Grimoire derives the rest. A theme with
  only one appearance always shows in it.
- Optional roles: `sidebar`, `surface`, `raised`, `text`, `textMuted`, `textFaint`, `border`,
  `accentForeground`, `userBubble`, `success`, `warning`, `danger`, and `dangerSurface`. Colors are hex.
- `css` runs after Grimoire's own styles. Use the stable hooks rather than class names, which can
  change: `[data-slot]` is set on `sidebar`, `sidebar-row`, `page`, `chat`, `chat-header`, `message`
  (with `data-role` of `user`, `assistant`, or `tools`), `composer`, and `dock`. The variables
  `--font-ui` and `--font-display` set the two typefaces. Remote files don't load: the app's content
  security policy blocks them.
- A file that doesn't load shows its error under the theme list in Settings.

## How it works

- **Processes.** Nothing starts at launch. Codex familiars share one `codex app-server` process (JSON-RPC
  over stdio), started on the first Codex message and stopped after 10 idle minutes. Each Claude
  familiar gets one `claude -p --input-format stream-json` process, started on its first message and
  stopped after 5 idle minutes. Closing Grimoire stops them all.
- **Sessions.** The Codex thread ID or Claude session ID is saved per familiar, so a restart resumes the
  same conversation. "New chat" drops it.
- **Access.** Workspace only: Codex runs in its `workspace-write` sandbox; Claude runs in
  `acceptEdits` mode, so it can edit files but shell commands are refused. Full access: Codex
  `danger-full-access`, Claude `bypassPermissions`. Neither asks for approval during a turn.
- **Streaming.** Text goes to the window at most every 50 ms and to SQLite every 750 ms and at the
  end of each turn. Markdown renders only once a message is finished.
- **Queue.** A message sent while the familiar works waits and runs next. Stop interrupts the turn
  (Claude through its interrupt request, Codex through `turn/interrupt`) and cancels the queue.
- **Familiar tools.** Grimoire passes each familiar an MCP server named `grimoire` (`--mcp-config` for
  Claude, thread config for Codex). The CLI starts `grimoire mcp <socket> <agent-id>`, which relays tool
  calls to the app over `grimoire.sock` in Grimoire's data folder (owner-only). The tools work in both
  access modes because they only touch Grimoire's own data.
- **Storage.** `~/Library/Application Support/dev.talix.grimoire/grimoire.db`, with user themes in
  `themes/` beside it. Default workspaces are under `~/Grimoire`. Deleting a familiar keeps its
  workspace folder.

## Checks

```sh
bun run test                     # timeline, models, chat store, moods, themes
cd src-tauri
cargo test                       # unit tests
cargo test -- --ignored          # live turns against the real codex and claude CLIs (costs tokens)
cargo clippy --all-targets -- -D warnings
```

## License

[MIT](LICENSE)
