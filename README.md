<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="src/assets/icon-night.svg">
    <img src="src/assets/icon-paper.svg" width="128" height="128" alt="Grimoire's icon: a red wax seal pressed with a familiar's face">
  </picture>
</p>

<h1 align="center">Grimoire</h1>

<p align="center">
  Persistent AI familiars for your Mac. Each one has a name, a face, a workspace folder, and runs on Codex or Claude.
</p>

<p align="center">
  <img src="docs/screenshots/chat.png" width="820" alt="A conversation with the Researcher familiar in the Tyrian theme">
</p>

## What it does

- **Familiars** keep their conversation across restarts. Each has an animated outline avatar that
  blinks, glances, and fidgets when idle, works while busy, smiles when it finishes, and frowns after
  an error.
- **Spells** chain familiars: research, then write, then review. Each step gets the last reply.
  Click a spell in the sidebar to cast it.
- **Rituals** send a familiar the same message on a schedule, like a morning summary.
- **Covens** group familiars. Home's summoning circle shows everyone or one coven at a time, with
  lines for the spells between familiars and gold arcs for their rituals.
- **Themes** come as seven built-in inks, and you can edit or write your own as JSON.
- **Familiars make things too.** Ask one for "a spell that…" or "a theme that feels like a rainy
  library" and it saves it through Grimoire's own tools, credited to it.
- **The vault** keeps logins, API keys, and cards in your macOS Keychain. Familiars can use a
  secret in a command without ever seeing it, and Grimoire asks you first.
- **The dock** pins your familiars to the edge of the screen, so you can chat without opening
  the main window.
- **Voice** dictation in the composer uses macOS speech recognition.

<table>
  <tr>
    <td width="50%"><img src="docs/screenshots/themes.png" alt="The theme picker in Settings, in the light Lapis theme"></td>
    <td width="50%"><img src="docs/screenshots/theme-editor.png" alt="The theme editor with color pickers and a live preview"></td>
  </tr>
  <tr>
    <td>Seven inks, each with a dark and a light version.</td>
    <td>Edit any color and watch the whole app change as you go.</td>
  </tr>
  <tr>
    <td width="50%"><img src="docs/screenshots/familiar-theme.png" alt="A theme called Rainy library, made by the Researcher familiar"></td>
    <td width="50%"><img src="docs/screenshots/cast-spell.png" alt="Casting the Topic brief spell"></td>
  </tr>
  <tr>
    <td>A theme a familiar made, with its own CSS.</td>
    <td>Casting a spell from the sidebar.</td>
  </tr>
</table>

## Run it

You need [Bun](https://bun.sh), Rust, and `codex` or `claude` installed and signed in. Grimoire reads
`PATH` from your login shell, so it finds them when you open it from Finder too.

```sh
bun install
bunx tauri dev                    # development
bunx tauri build --bundles app    # src-tauri/target/release/bundle/macos/Grimoire.app
```

Shortcuts: ⌘K opens the command palette, ⌘N summons a familiar, ⌘, opens Settings, and ⌘1 to ⌘9 open
a familiar.

## Familiars

Build a familiar's avatar in the avatar studio: one of six shapes (orbit, spark, cloud, drop, ears,
classic), three eye styles, and any color. Each shape has its own moves, like the orbit's moon or
the cloud's drizzle, and all motion stops when macOS Reduce Motion is on. The avatar's color tints
that familiar's chat.

The gear in the chat header changes a familiar's avatar, name, model, effort, or access, or deletes
it. The model chip in the composer shows the model it is running and switches it.

**Access.** "Workspace only" keeps Codex in its `workspace-write` sandbox and runs Claude in
`acceptEdits` mode, so it can edit files but shell commands are refused. "Full access" uses Codex
`danger-full-access` and Claude `bypassPermissions`. Neither asks for approval during a turn.

## Chats and the palette

Each familiar keeps a list of chats. The chat header shows the current chat's title, which comes from
the first message you send in it. Click the title to filter past chats, open one, rename or delete it,
or start a new chat (the pencil button does that too). Opening an older chat picks its conversation
back up: your next message resumes that chat's Codex thread or Claude session. Deleting the open chat
switches to the newest one left. You can't switch chats while the familiar is replying. Rituals and
spells post into the familiar's current chat.

⌘K opens the command palette anywhere in the main window. Type part of a name to jump to a familiar,
cast a spell, open Home, Rituals, or a Settings section, or run an action: summon a familiar, start a
new spell, or start a new chat with the familiar you're in. From two letters on, it also searches the
text of every message you sent and every reply, and shows the familiar, the chat, and the matching
words. Picking a message opens its chat and scrolls to it. Arrow keys move, Enter opens, Esc closes.

## Covens and the circle

Home seats your familiars around a summoning circle. A curved line joins two familiars for each spell
that hands work between them (hover it for the name), a gold arc on the outer ring marks a familiar
with rituals, and a working familiar glows with a line from the center.

A coven is a named group of familiars, and each familiar sits in one at most. The chips above the
circle switch between everyone and one coven; "All" seats each coven together under a labeled arc.
Create a coven with the + chip, change its name and familiars with "Edit", or pick a familiar's coven
in its settings. The sidebar groups familiars by coven, with the rest under "Unbound", and
`list_familiars` tells familiars which coven each one is in.

## Spells and rituals

A spell is a list of steps, each sent to one familiar, which waits for the reply before the next.
In a step, `{{input}}` is the text you cast it with and `{{previous}}` is the last reply; the last
reply is added at the end when a step doesn't use it.

A ritual sends a prompt every few hours, daily at a time, or on chosen weekdays. One timer sleeps
until the next run, and a run missed while Grimoire was closed fires once at the next launch.

## Vault

Settings, Vault holds logins, API keys, and cards. Secret values go in your macOS Keychain under
`dev.talix.grimoire.vault`; Grimoire's database stores only the name, site, username, and a card's
last four digits.

Familiars can find items with `vault_search` (no secrets) and use one with `vault_use`:

1. Grimoire asks you in the corner of the window: "Researcher wants to use the password for
   github.com". Allow once, Always allow, or Deny. No answer in 2 minutes counts as Deny.
2. On yes, Grimoire puts the secret in a one-time named pipe and gives the familiar only its path.
   The familiar reads it inside the command that needs it, like `curl -H "Authorization: Bearer
   $(cat <path>)"`, so the value never enters the conversation sent to OpenAI or Anthropic.
3. The pipe gives out the secret once, then disappears. Unread, it expires after 2 minutes.

A familiar with shell access could still print what it reads, so Grimoire tells familiars never to,
and every request shows in the item's history. An item's page lists the familiars you've always
allowed, with a button to make them ask again.

## Permissions

Settings, Permissions decides when familiars need your OK before a shell command:

- **Never ask** (the default): familiars act on their own, within the access you gave each one.
- **Ask when a rule matches**: Grimoire asks when a command matches an "Always ask" rule. It starts
  with `git push*`, `rm -rf *`, and `sudo *`.
- **Ask before every command**: Grimoire asks unless an "Allow without asking" rule matches. An
  "Always ask" rule wins when both match.

In a rule, `*` matches anything. "Always allow" on a prompt adds an allow rule for that exact
command. Codex asks through its approval requests (Grimoire sets its approval policy to
`untrusted`, so commands it knows only read run without asking); Claude asks through a `PreToolUse`
hook on Bash that Grimoire registers, which runs even for commands your own Claude settings allow.

## Usage

Settings, Usage shows your plan limits and what Grimoire itself used:

- **Codex**: the 5-hour and weekly limits from `account/rateLimits/read`. Opening the page starts
  `codex app-server` if it isn't running; it stops again after 10 idle minutes.
- **Claude**: Claude only reports limits while it replies (`rate_limit_event`), so the page shows the
  latest snapshot and how old it is.
- **Grimoire, last 7 days**: tokens per turn, input and output together, by source (you, rituals,
  spells) or by familiar, with a daily chart and a table of the numbers.

## Updates

Settings, Updates checks this repository's GitHub releases (the latest release's tag against the
app's version), once a day at launch unless you turn that off, or whenever you click "Check for
updates". A newer version shows in the sidebar and as a Download button that opens the release.
Installing it is a download for now; automatic installs need signed release builds.

## Themes

Pick a theme in Settings. The seven built-ins are named for old inks and pigments: Tyrian (the
default), Iron gall, Verdigris, Dragon's blood, Orpiment, Lapis, and Moonstone. Mode (system, light,
or dark) is a separate setting.

Your own themes are JSON files in `~/Library/Application Support/dev.talix.grimoire/themes`.
Settings opens the folder, and Grimoire reloads it whenever its window comes back to the front, so
any text editor works. "Make a copy to edit" opens the same file in the built-in editor.

```json
{
  "version": 1,
  "name": "Rainy library",
  "colors": { "accent": "#8fb3c9" },
  "variants": {
    "dark": { "canvas": "#14181c" },
    "light": { "canvas": "#f3f5f6", "accent": "#2f6d8f" }
  },
  "css": "[data-slot=\"sidebar\"] { border-right: 2px solid var(--accent); }"
}
```

- `colors` applies to both appearances, and `variants.dark` or `variants.light` override it. Each
  appearance needs `canvas` (the background) and `accent`; Grimoire derives the rest. A theme with
  only one appearance always shows in it.
- Optional roles: `sidebar`, `surface`, `raised`, `text`, `textMuted`, `textFaint`, `border`,
  `accentForeground`, `userBubble`, `success`, `warning`, `danger`, and `dangerSurface`, all in hex.
- `css` runs after Grimoire's own styles. Target the stable hooks, not class names: `[data-slot]` is
  set on `sidebar`, `sidebar-row`, `page`, `chat`, `chat-header`, `message` (with `data-role` of
  `user`, `assistant`, or `tools`), `composer`, and `dock`. `--font-ui` and `--font-display` set the
  two typefaces. Remote files don't load, because the app's content security policy blocks them.
- A file that fails to load shows its error under the theme list.

## App icons

Grimoire has two icons, a red wax seal on paper and the same seal on dark ink. Settings switches
between them or matches the app's appearance. The Dock shows your choice while Grimoire is open;
Finder and a closed Dock icon show the paper one, which is the bundle icon.

The icons are drawn in `scripts/icons.mjs`. To change them:

```sh
bun scripts/icons.mjs     # writes src/assets/icon-paper.svg and icon-night.svg
bunx @resvg/resvg-js-cli --fit-width 1024 src/assets/icon-paper.svg app-icon.png
bunx @resvg/resvg-js-cli --fit-width 512 src/assets/icon-paper.svg src-tauri/icons/app-paper.png
bunx @resvg/resvg-js-cli --fit-width 512 src/assets/icon-night.svg src-tauri/icons/app-night.png
bunx tauri icon app-icon.png    # then delete the android and ios folders it adds
```

## How it works

<details>
<summary>Processes, sessions, storage, and the familiar tools</summary>

- **Processes.** Nothing starts at launch. Codex familiars share one `codex app-server` process
  (JSON-RPC over stdio), started on the first Codex message and stopped after 10 idle minutes. Each
  Claude familiar gets one `claude -p --input-format stream-json` process, started on its first
  message and stopped after 5 idle minutes. Closing Grimoire stops them all.
- **Sessions.** Each chat saves its Codex thread ID or Claude session ID, so a restart resumes the
  same conversation. "New chat" starts a chat with no session; opening an older chat loads its
  session for the next turn. The current chat's session is also kept on the familiar, which is what
  the providers resume.
- **Search.** Messages are indexed in an SQLite FTS5 table that triggers keep in sync as messages are
  added, streamed, and deleted. Tool and error rows aren't indexed.
- **Streaming.** Text goes to the window at most every 50 ms and to SQLite every 750 ms and at the
  end of each turn. Markdown renders once a message is finished.
- **Queue.** A message sent while a familiar works waits and runs next. Stop interrupts the turn
  (Claude through its interrupt request, Codex through `turn/interrupt`) and cancels the queue.
- **Familiar tools.** Grimoire gives each familiar an MCP server named `grimoire` (`--mcp-config`
  for Claude, thread config for Codex) with `list_familiars`, `list_spells`, `create_spell`,
  `list_themes`, `create_theme`, `set_theme`, `vault_search`, and `vault_use`. The CLI starts `grimoire mcp <socket> <agent-id>`,
  which relays calls to the app over `grimoire.sock` in its data folder (owner-only). Familiars can't
  cast, edit, or delete spells.
- **Claude billing.** With "Claude subscription" (the default), Grimoire removes
  `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`, and `ANTHROPIC_BASE_URL` from the `claude` process
  so it uses your claude.ai Pro or Max sign-in. "API key" leaves them in place.
- **Storage.** `~/Library/Application Support/dev.talix.grimoire/grimoire.db`, with your themes in
  `themes/` beside it. Default workspaces go under `~/Grimoire`. Deleting a familiar keeps its
  workspace folder.
- **Names.** The code calls a familiar an agent, a spell a workflow, and a ritual a routine.

</details>

## Checks

```sh
bun run test                     # timeline, models, chat store, palette ranking, moods, themes
cd src-tauri
cargo test                       # unit tests
cargo test -- --ignored          # live turns against the real codex and claude CLIs (costs tokens)
cargo clippy --all-targets -- -D warnings
```

## License

[MIT](LICENSE)
