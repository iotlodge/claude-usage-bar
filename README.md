# claude-usage-bar

A tiny native macOS **menu bar widget** that shows **every** Claude usage limit at a glance — the current **5-hour session**, the **weekly cap across all models**, and any **per-model weekly cap** your plan has (Fable, Opus, …) — each with a countdown to its reset. Written in Rust. No telemetry, no dependencies beyond the binary.

```
🟢 5h 12% · 7d 28% · Fable 4%     ← menu bar title
🟠 5h 53% · 7d 28% · Fable 61%    ← orange at ≥50% on any limit
🔴 5h 3%  · 7d 75% · Fable 100%   ← red at ≥80% on any limit
```

The dot reflects the **worst** limit, not just the session — a maxed-out per-model cap turns it red even when the session is nearly idle.

Clicking the item opens a dropdown with the details:

```
🟢 Current session: 3% · resets in 4h 55m
🟠 All models: 75% · resets in 5h 35m
🔴 Fable (weekly): 100% · resets in 5h 35m
────────────────────────────
Updated 9:47 AM
Refresh Now
Open claude.ai Usage Page
────────────────────────────
Quit Claude Usage Bar
```

The rows are built from whatever the API reports, so a new per-model cap appears on its own without a code change.

This is a Rust/macOS port of the excellent [Defacedz/claude-usage-widget](https://github.com/Defacedz/claude-usage-widget) (C#/WPF for Windows, with a Swift variant), which pioneered the approach. MIT-licensed like the original.

## How it works

The widget rides on your existing **Claude Code** login — you must be signed in to Claude Code (`claude` CLI) on this Mac.

1. **Credentials** — reads the Claude Code OAuth token from the login keychain (generic password `"Claude Code-credentials"`, via `/usr/bin/security`), falling back to `~/.claude/.credentials.json` if present.
2. **Token refresh** — when the access token is near expiry, it refreshes it against `platform.claude.com` / `console.anthropic.com` using Claude Code's own client ID, and **writes the rotated tokens back** to the keychain so Claude Code stays logged in.
3. **Usage** — polls `https://api.anthropic.com/api/oauth/usage` (the same endpoint the Claude apps use) every 5 minutes and reads its `limits` array, which carries a `percent`, `severity` and `resets_at` for each limit in force: `session`, `weekly_all`, and one `weekly_scoped` entry per model-specific cap (the model name comes from `scope.model.display_name`). If a response arrives without a `limits` array, it falls back to the older top-level `five_hour` / `seven_day` / `seven_day_opus` keys.

   > Per-model caps live **only** in the `limits` array — they have no top-level key. Reading just the top-level keys hides them, which is how a maxed-out model cap can go unnoticed until it bites.

Your token is sent to `api.anthropic.com`, `platform.claude.com` and `console.anthropic.com` — nowhere else. Nothing is logged or uploaded.

> ⚠️ The usage endpoint is not a documented public API. It can change or disappear without notice.

## Build & run

Requires a Rust toolchain (`rustup`).

```sh
cargo build --release
./target/release/claude-usage-bar &
```

The first keychain read may pop a macOS dialog asking to allow access to *Claude Code-credentials* — click **Always Allow** to never see it again.

### One-shot mode (no GUI)

Handy for scripts or debugging:

```sh
$ claude-usage-bar --once
🟢 Current session: 3% · resets in 4h 55m
🟠 All models: 75% · resets in 5h 35m
🔴 Fable (weekly): 100% · resets in 5h 35m
```

### Start at login

Install the binary somewhere stable and register a launchd agent:

```sh
mkdir -p ~/.local/bin
cp target/release/claude-usage-bar ~/.local/bin/

cat > ~/Library/LaunchAgents/com.claude-usage-bar.plist <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>com.claude-usage-bar</string>
  <key>ProgramArguments</key><array><string>$HOME/.local/bin/claude-usage-bar</string></array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
</dict></plist>
EOF

launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/com.claude-usage-bar.plist
```

To stop it: `launchctl bootout gui/$(id -u)/com.claude-usage-bar`.

## Architecture

Single file, `src/main.rs` (~400 lines):

| Piece | What it does |
|---|---|
| `tray-icon` + `tao` | Native `NSStatusItem` menu bar UI; the app runs with `ActivationPolicy::Accessory`, so no Dock icon. |
| credentials module | Keychain read/write via `/usr/bin/security` (same approach as the upstream Swift widget); unknown JSON fields are preserved on write-back. |
| API module | Token refresh + usage fetch over `ureq` (rustls, no OpenSSL). |
| worker thread | Fetches immediately at launch, then every 5 minutes; "Refresh Now" pokes it over an `mpsc` channel; results land in the UI thread via the `tao` event-loop proxy. |

Errors (offline, signed out, endpoint change) are non-fatal: the title switches to `⚠️ Claude` and the dropdown shows the error until a later poll succeeds.

## Credits

- [Defacedz/claude-usage-widget](https://github.com/Defacedz/claude-usage-widget) — the original Windows widget and Swift mac variant this is ported from (MIT).
