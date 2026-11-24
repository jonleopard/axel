# RFD 0003: Axel as a Rex-native citizen

State: discussion
Authors: Ludo, astra  ·  Started: 2026-11-22

## Problem

Axel uses Rex as a guest does. It runs a TUI in a terminal block, shells out to
`rex api call` for each layout change, and reports status with OSC 7501. That
works, but it uses a fraction of what Rex now offers. It also leaves Axel
looking like "a program in a pane" rather than part of the workbench.

Rex moved from API revision 7 to 19 between our first study and 2026-11-22
(`docs/RESEARCH.md`, Rex section). The new parts are the ones an orchestrator
needs:

- Prompts that reach every connected Rex app, the iOS app included, where the
  first answer wins (`rex.ui.input`, `rex.ui.pick`).
- Actions that one client offers and any other client calls
  (`client.keymap` with `actions`, `client.perform`).
- A control connection that resumes, negotiates revisions and streams every
  event (`session.bootstrap`, `reconnect_token`).
- Callers whose identity the server verifies, as a Unix uid or a Tailscale
  login and tags (`principal`).
- Block plugins: executable programs that add creators (`rex server run
  --block-dir`). The protocol is not published yet.

The question: what deeper integration does this make possible that no other
agent tool can have, and in what order do we build it?

## Sketch

### The split

Rex owns *where* work runs and *how it appears*: processes, panes, devices,
people's screens. Axel owns *who* acts, *why*, and *what was done*: agent
identities, threads, the signed log, policy and autopilot. Rex has no concept
of an agent identity, a thread that outlives its panes, or a signed history.
Axel never keeps its truth in Rex. Rex is the surface; the log stays in
`~/.axel/axel.db`.

### Levels, each usable on its own

| Level | What Axel does | Rex surface | State |
|---|---|---|---|
| 1. Guest | TUI in a terminal block, CLI calls, OSC 7501 | `rex api call`, OSC 7501 | shipped |
| 2. Native client | one long-lived control connection per environment | `/api/control`, `session.bootstrap`, events | spiked, works |
| 3. Part of the interface | Axel's verbs in the palette, keys and iOS; owner prompts on every device | `client.keymap` actions, `client.perform`, `rex.ui` | spiked, works with an `init.lua` bridge |
| 4. Own blocks | thread, inbox and dashboard blocks with methods and events | block plugins | protocol unpublished |
| 5. Shared runtime | several people's agents on one Rex server, identities mapped | principals, shared and login server, hosts | design only |

### Level 3 in detail: the owner's remote control

This is the part with no equivalent elsewhere. An agent asks a permission
question; within a second it appears in the Rex command palette on every Mac
and iPhone the owner has connected. They answer on whichever is nearest. The
answer is signed into the thread as a `permission_reply` by the owner.

Two paths, both tested on 2026-11-22:

1. **Rex asks, Axel listens.** A small Axel section in `init.lua` defines
   `axel_ask`. Axel runs `rex do axel_ask title=… options="Approve|Deny"`
   (or `client.action` on its connection) and gets the answer as JSON. Our
   test prompt was answered from a Mac in 6.5 s.
2. **Rex calls Axel.** Axel's connection declares actions such as
   `axel.new_thread`, `axel.reply`, `axel.approve`. Any Lua action, key
   binding or `rex -C axel do axel.reply …` reaches Axel through
   `client.perform`, with the caller's principal filled in by the server so it
   can't be faked. A Lua action can combine both: ask with `rex.ui.pick`
   ("which thread?"), then `client.perform` to Axel.

Together they give a full loop with no Axel window open: notice, ask, answer,
act, from any device signed in to the owner's tailnet.

### Level 2 in detail: one connection, live state

`crates/rex` keeps its CLI runner for one-shot commands. A new control
connection, owned by whatever long-lived Axel process exists (the TUI today,
a daemon later):

- Sends `hello` with `api_revision` 19 and `api_min_revision` 17, the first
  revision with prompts.
- Calls `session.bootstrap` with `event_scope: all` and the sessions Axel
  owns, then follows `program_status_changed`, `process_changed`,
  `child_exited` and `block_closed` instead of polling `session.view`.
- Keeps the newest `reconnect_token` so a restart keeps the same client ID,
  and with it the actions other clients address.

The spike is `crates/rex/examples/rex_control_probe.rs` on the
`spike/rex-native` branch. It uses `tungstenite` over the Unix socket,
synchronously, with no async runtime.

### Level 4: blocks

If plugin blocks can draw their own content, Axel ships a plugin with
flavors such as `dev.axel.thread`, `dev.axel.inbox` and `dev.axel.dashboard`.
Their methods (`send`, `approve`, `read`) and events (`message_posted`,
`permission_requested`) become callable from Lua, the CLI and other plugins.
Rex restores the blocks after a restart and shows them on every device. Axel
would stop being a TUI in a pane and become a kind of block.

## Open questions

- **What does a plugin block draw?** Its own pixels, a terminal surface, or
  structured content the apps render? This decides whether level 4 is worth
  more than level 3. Find out: ask Superlogical for the plugin protocol, and
  read the "Block Plugins" docs page when it is published.
- **When do connection-script actions run?** On 2026-11-22 `client.set_script`
  accepted a `rex.action` but warned "Actions in a client script run from a
  later version of Rex". Once they run, Axel can define `axel_ask` on its own
  connection and stop touching the owner's `init.lua`. Find out: retry the
  probe with `--ask` on each Rex release.
- **Does Rex.app list other clients' actions in its palette?** Today it lists
  configuration-file actions. If it lists declared client actions too, the
  `init.lua` wrappers disappear. Find out: open the palette while the probe runs.
- **Who answers when the owner is away?** A prompt that times out returns
  `timeout`. Autopilot policy decides what happens next, not Rex.
- **Identity mapping.** A Tailscale principal (`user_id`, `tags`) could be
  attested by the owner's Ed25519 key, so Rex callers map to Axel identities.
  Find out: sketch the attestation in `docs/CONTRACTS.md` before level 5.
- **Prompt size.** `rex.ui.pick` allows 2,000 items and `allow_other` for free
  text. Is a permission request with a diff summary readable in the palette?
  Find out: try a real Claude Code permission request.

## Risks

- **Rex is closed source and in preview.** Revisions and the plugin protocol
  can change. Mitigation: the server translates old revisions, we pin
  `api_min_revision`, and Axel keeps working at level 1 without any of this.
- **Overlap with Rex's own roadmap.** Superlogical lists a unified inbox.
  Simple status should flow into it through OSC 7501, not compete with it.
  Axel's value is identity, threads and the log, which Rex has no reason to build.
- **Editing the owner's `init.lua`.** Until connection scripts run actions,
  level 3 needs an Axel section in the owner's file. It must be clearly fenced,
  idempotent, and removable with one command.
- **Prompt fatigue.** Every connected device lights up. Ask only for decisions
  autopilot can't make, and batch them.

## What we learned

- 2026-11-22: Rex 0.1.0-preview.22, API revision 19 on the laptop and the mini.
  The control connection negotiated revision 19. `session.bootstrap` with
  `event_scope: all` delivered `client_joined`, `client_left`,
  `client_performed` and `com.superlogical.terminal.ports_changed` without
  attaching any session.
- 2026-11-22: An `init.lua` action calling `rex.ui.pick`, run with `rex do`,
  showed a prompt in Rex.app and returned the owner's choice (`Approve`) in 6.5 s.
- 2026-11-22: A control connection declared `axel.ping` with `client.keymap`.
  Both `rex -C axel-probe do axel.ping` and a Lua `client.perform` reached it
  and got its reply. `rex -C axel-probe do axel.nope` was refused by the CLI.
- 2026-11-22: `client.set_script` loads a `rex.action` but the server refuses to
  run it by name: "the configuration file defines no action". Client-script
  actions are announced for a later release.
- 2026-11-22: Rex.app 0.1.0 ships Quick Look (`client.quick_look`): it previews
  files and web links visible in the active pane, downloading remote files
  first. There is no web view or browser block in the app or server. The
  protocol has a `web` client platform and the server takes
  `--websocket-origin-pattern`, so a browser client looks planned.
- 2026-11-22: No plugin registry exists in the API, CLI, binary or docs. The
  docs sidebar shows "Block Plugins" under "Build for Rex" with no page yet.
- 2026-11-22: A script run with `rex do SCRIPT --action NAME` may define an action, but
  `rex.ui` there returns `unsupported`: only configuration-file actions ask. Killing the
  `rex` process that waits on an action closes its prompt on every app; a second prompt
  right after was shown, not refused as `busy`.
- 2026-11-22: Level 3 shipped for questions. `axel rex setup` installs `axel.lua`
  (action `axel_ask`) behind a fenced `require("axel")` in `init.lua`; the composer
  puts each question through it (`axel thread ask`), falling back to the floating
  pane when Rex can't reach anyone. See `docs/DESIGN.md`, "Questions".
