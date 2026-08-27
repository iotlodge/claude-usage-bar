# claude-usage-bar

A tiny native macOS **menu bar widget** that shows your Claude usage limits at a glance — the **5-hour session** and the **weekly** limit — with countdowns to each reset. Written in Rust. No telemetry, no dependencies beyond the binary.

```
🟢 12% · 28%        ← menu bar title: 5-hour · weekly
🟠 53% · 28%        ← orange at ≥50% on either limit
🔴 91% · 74%        ← red at ≥80%
```

Clicking the item opens a dropdown with the details:

```
5-hour session: 53% · resets in 3h 12m
Weekly (all models): 28% · resets Sun 2:59 PM
────────────────────────────
Updated 9:47 AM
Refresh Now
Open claude.ai Usage Page
────────────────────────────
Quit Claude Usage Bar
```

This is a Rust/macOS port of the excellent [Defacedz/claude-usage-widget](https://github.com/Defacedz/claude-usage-widget) (C#/WPF for Windows, with a Swift variant), which pioneered the approach. MIT-licensed like the original.

## How it works

The widget rides on your existing **Claude Code** login — you must be signed in to Claude Code (`claude` CLI) on this Mac.

1. **Credentials** — reads the Claude Code OAuth token from the login keychain (generic password `"Claude Code-credentials"`, via `/usr/bin/security`), falling back to `~/.claude/.credentials.json` if present.
2. **Token refresh** — when the access token is near expiry, it refreshes it against `platform.claude.com` / `console.anthropic.com` using Claude Code's own client ID, and **writes the rotated tokens back** to the keychain so Claude Code stays logged in.
3. **Usage** — polls `https://api.anthropic.com/api/oauth/usage` (the same endpoint the Claude apps use) every 5 minutes, which returns `utilization` (percent) and `resets_at` for the `five_hour` and `seven_day` limits (plus a separate Opus weekly limit on plans that have one — shown automatically when present).

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
5-hour session: 53% · resets in 3h 12m
Weekly (all models): 28% · resets Sun 2:59 PM
```

### Start at login

Install the binary somewhere stable and register a launchd agent:

```sh
cp target/release/claude-usage-bar /usr/local/bin/

cat > ~/Library/LaunchAgents/com.claude-usage-bar.plist <<'EOF'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>com.claude-usage-bar</string>
  <key>ProgramArguments</key><array><string>/usr/local/bin/claude-usage-bar</string></array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
</dict></plist>
EOF

launchctl load ~/Library/LaunchAgents/com.claude-usage-bar.plist
```

To stop it: `launchctl unload ~/Library/LaunchAgents/com.claude-usage-bar.plist`.

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
