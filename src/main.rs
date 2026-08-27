// claude-usage-bar — macOS menu bar widget showing Claude's 5-hour session
// and weekly usage limits, with reset countdowns.
//
// Reads the Claude Code OAuth token from the login keychain (generic password
// "Claude Code-credentials", via /usr/bin/security) with a fallback to
// ~/.claude/.credentials.json, refreshes it when expired (writing the rotated
// tokens back so Claude Code stays logged in), and polls the undocumented
// https://api.anthropic.com/api/oauth/usage endpoint. Tokens go to
// api.anthropic.com, platform.claude.com and console.anthropic.com — nowhere
// else. No telemetry.
//
// Ported from https://github.com/Defacedz/claude-usage-widget (MIT).

use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{TrayIcon, TrayIconBuilder};

const KEYCHAIN_SERVICE: &str = "Claude Code-credentials";
const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const TOKEN_URLS: [&str; 2] = [
    "https://platform.claude.com/v1/oauth/token",
    "https://console.anthropic.com/v1/oauth/token",
];
const POLL_INTERVAL: Duration = Duration::from_secs(300);

#[derive(Clone, Debug, Default)]
struct LimitInfo {
    utilization: Option<f64>,
    resets_at: Option<String>,
}

#[derive(Clone, Debug, Default)]
struct Usage {
    five_hour: Option<LimitInfo>,
    seven_day: Option<LimitInfo>,
    seven_day_opus: Option<LimitInfo>,
}

enum UserEvent {
    Menu(MenuEvent),
    Usage(Result<Usage, String>),
}

// ---------- credentials (login keychain, via /usr/bin/security) ----------

enum CredSource {
    Keychain,
    File(PathBuf),
}

fn security(args: &[&str]) -> (bool, String) {
    match Command::new("/usr/bin/security").args(args).output() {
        Ok(out) => (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).to_string(),
        ),
        Err(_) => (false, String::new()),
    }
}

fn read_credentials() -> Option<(String, CredSource)> {
    let (ok, out) = security(&["find-generic-password", "-s", KEYCHAIN_SERVICE, "-w"]);
    if ok {
        let s = out.trim().to_string();
        if !s.is_empty() {
            return Some((s, CredSource::Keychain));
        }
    }
    // fallback: some setups keep the plain file like on Windows/Linux
    let path = PathBuf::from(std::env::var("HOME").ok()?).join(".claude/.credentials.json");
    let raw = std::fs::read_to_string(&path).ok()?;
    Some((raw, CredSource::File(path)))
}

fn keychain_account() -> String {
    let (_, out) = security(&["find-generic-password", "-s", KEYCHAIN_SERVICE]);
    for line in out.lines() {
        if line.contains("\"acct\"") {
            if let Some(acct) = line.split('"').nth(3) {
                return acct.to_string();
            }
        }
    }
    std::env::var("USER").unwrap_or_else(|_| "claude".into())
}

fn write_credentials(raw: &str, source: &CredSource) {
    match source {
        // -U updates the existing item in place
        CredSource::Keychain => {
            let acct = keychain_account();
            let _ = security(&[
                "add-generic-password",
                "-U",
                "-s",
                KEYCHAIN_SERVICE,
                "-a",
                &acct,
                "-w",
                raw,
            ]);
        }
        CredSource::File(path) => {
            let _ = std::fs::write(path, raw);
        }
    }
}

// ---------- Claude API ----------

fn now_ms() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as f64)
        .unwrap_or(0.0)
}

fn access_token(agent: &ureq::Agent) -> Result<String, String> {
    let (raw, source) =
        read_credentials().ok_or("not signed in — run `claude` and log in first")?;
    let mut root: Value =
        serde_json::from_str(&raw).map_err(|_| "could not parse Claude Code credentials")?;
    let oauth = root
        .get("claudeAiOauth")
        .ok_or("no claudeAiOauth in credentials")?;
    let access = oauth
        .get("accessToken")
        .and_then(Value::as_str)
        .ok_or("no access token in credentials")?
        .to_string();
    let refresh = oauth
        .get("refreshToken")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let expires_at = oauth.get("expiresAt").and_then(Value::as_f64).unwrap_or(0.0);

    if expires_at > 0.0 && now_ms() < expires_at - 120_000.0 {
        return Ok(access);
    }
    if refresh.is_empty() {
        return Ok(access);
    }

    let form = format!("grant_type=refresh_token&refresh_token={refresh}&client_id={CLIENT_ID}");
    let json = serde_json::json!({
        "grant_type": "refresh_token",
        "refresh_token": refresh,
        "client_id": CLIENT_ID,
    })
    .to_string();

    for url in TOKEN_URLS {
        for (body, content_type) in [
            (&form, "application/x-www-form-urlencoded"),
            (&json, "application/json"),
        ] {
            let Ok(resp) = agent.post(url).set("Content-Type", content_type).send_string(body)
            else {
                continue;
            };
            let Ok(v) = resp.into_json::<Value>() else { continue };
            let Some(new_access) = v.get("access_token").and_then(Value::as_str) else {
                continue;
            };
            let new_refresh = v
                .get("refresh_token")
                .and_then(Value::as_str)
                .unwrap_or(&refresh)
                .to_string();
            let expires_in = v.get("expires_in").and_then(Value::as_f64).unwrap_or(0.0);
            // Write the rotated tokens back so Claude Code stays logged in,
            // preserving any fields we don't know about.
            if let Some(o) = root.get_mut("claudeAiOauth").and_then(Value::as_object_mut) {
                o.insert("accessToken".into(), Value::from(new_access));
                o.insert("refreshToken".into(), Value::from(new_refresh));
                o.insert(
                    "expiresAt".into(),
                    Value::from((now_ms() + expires_in * 1000.0) as u64),
                );
                write_credentials(&root.to_string(), &source);
            }
            return Ok(new_access.to_string());
        }
    }
    Ok(access) // last resort, same as the Windows/Swift widgets
}

fn fetch_usage(agent: &ureq::Agent) -> Result<Usage, String> {
    let token = access_token(agent)?;
    let resp = agent
        .get(USAGE_URL)
        .set("Authorization", &format!("Bearer {token}"))
        .set("anthropic-beta", "oauth-2025-04-20")
        .call()
        .map_err(|e| match e {
            ureq::Error::Status(code, _) => format!("usage endpoint returned HTTP {code}"),
            e => format!("network error: {e}"),
        })?;
    let v: Value = resp
        .into_json()
        .map_err(|_| "bad response from usage endpoint".to_string())?;
    let limit = |key: &str| -> Option<LimitInfo> {
        let o = v.get(key)?.as_object()?;
        Some(LimitInfo {
            utilization: o.get("utilization").and_then(Value::as_f64),
            resets_at: o
                .get("resets_at")
                .and_then(Value::as_str)
                .map(String::from),
        })
    };
    Ok(Usage {
        five_hour: limit("five_hour"),
        seven_day: limit("seven_day"),
        seven_day_opus: limit("seven_day_opus"),
    })
}

// ---------- formatting ----------

fn fmt_pct(v: Option<f64>) -> String {
    match v {
        Some(v) => format!("{:.0}%", v),
        None => "–".into(),
    }
}

fn dot(worst: f64) -> &'static str {
    if worst >= 80.0 {
        "🔴"
    } else if worst >= 50.0 {
        "🟠"
    } else {
        "🟢"
    }
}

fn fmt_reset(resets_at: &Option<String>) -> String {
    let Some(s) = resets_at else { return String::new() };
    let Ok(t) = chrono::DateTime::parse_from_rfc3339(s) else {
        return String::new();
    };
    let t = t.with_timezone(&chrono::Local);
    let d = t - chrono::Local::now();
    if d.num_seconds() <= 0 {
        return " · resets soon".into();
    }
    if d.num_hours() < 24 {
        format!(" · resets in {}h {:02}m", d.num_hours(), d.num_minutes() % 60)
    } else {
        format!(" · resets {}", t.format("%a %-I:%M %p"))
    }
}

fn limit_line(label: &str, l: &Option<LimitInfo>) -> String {
    match l {
        Some(l) => format!("{label}: {}{}", fmt_pct(l.utilization), fmt_reset(&l.resets_at)),
        None => format!("{label}: –"),
    }
}

// ---------- app ----------

fn main() {
    // `claude-usage-bar --once` prints the usage to stdout and exits.
    if std::env::args().any(|a| a == "--once") {
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(25))
            .build();
        match fetch_usage(&agent) {
            Ok(u) => {
                println!("{}", limit_line("5-hour session", &u.five_hour));
                println!("{}", limit_line("Weekly (all models)", &u.seven_day));
                if u.seven_day_opus.is_some() {
                    println!("{}", limit_line("Weekly (Opus)", &u.seven_day_opus));
                }
            }
            Err(e) => {
                eprintln!("Error: {e}");
                std::process::exit(1);
            }
        }
        return;
    }

    let mut event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    event_loop.set_activation_policy(ActivationPolicy::Accessory); // no Dock icon

    let proxy = event_loop.create_proxy();
    MenuEvent::set_event_handler(Some(move |event| {
        let _ = proxy.send_event(UserEvent::Menu(event));
    }));

    let menu = Menu::new();
    let item_five = MenuItem::new("5-hour session: …", false, None);
    let item_week = MenuItem::new("Weekly (all models): …", false, None);
    let item_opus = MenuItem::new("Weekly (Opus): …", false, None);
    let item_updated = MenuItem::new("Waiting for first update…", false, None);
    let item_refresh = MenuItem::new("Refresh Now", true, None);
    let item_open = MenuItem::new("Open claude.ai Usage Page", true, None);
    let item_quit = MenuItem::new("Quit Claude Usage Bar", true, None);
    menu.append_items(&[
        &item_five,
        &item_week,
        &item_opus,
        &PredefinedMenuItem::separator(),
        &item_updated,
        &item_refresh,
        &item_open,
        &PredefinedMenuItem::separator(),
        &item_quit,
    ])
    .expect("failed to build menu");
    let mut opus_present = true; // removed from the menu if the API never reports it

    // Worker: fetch immediately, then every POLL_INTERVAL or when poked by Refresh.
    let (poke_tx, poke_rx) = mpsc::channel::<()>();
    let worker_proxy = event_loop.create_proxy();
    std::thread::spawn(move || {
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(25))
            .build();
        loop {
            let result = fetch_usage(&agent);
            if worker_proxy.send_event(UserEvent::Usage(result)).is_err() {
                break; // event loop is gone
            }
            match poke_rx.recv_timeout(POLL_INTERVAL) {
                Ok(()) | Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    });

    let mut tray: Option<TrayIcon> = None;

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;

        match event {
            // Create the tray only once the event loop is running
            // (https://github.com/tauri-apps/tray-icon/issues/90)
            Event::NewEvents(StartCause::Init) => {
                tray = Some(
                    TrayIconBuilder::new()
                        .with_menu(Box::new(menu.clone()))
                        .with_title("⏳ Claude")
                        .with_tooltip("Claude usage")
                        .build()
                        .expect("failed to create menu bar item"),
                );
                // Tao only exposes redraw on windows; wake the run loop so the
                // item actually appears.
                if let Some(rl) = objc2_core_foundation::CFRunLoop::main() {
                    objc2_core_foundation::CFRunLoop::wake_up(&rl);
                }
            }

            Event::UserEvent(UserEvent::Usage(result)) => match result {
                Ok(u) => {
                    let five = u.five_hour.as_ref().and_then(|l| l.utilization);
                    let week = u.seven_day.as_ref().and_then(|l| l.utilization);
                    let opus = u.seven_day_opus.as_ref().and_then(|l| l.utilization);
                    let worst = five
                        .unwrap_or(0.0)
                        .max(week.unwrap_or(0.0))
                        .max(opus.unwrap_or(0.0));
                    if let Some(t) = &tray {
                        t.set_title(Some(format!(
                            "{} {} · {}",
                            dot(worst),
                            fmt_pct(five),
                            fmt_pct(week)
                        )));
                    }
                    item_five.set_text(limit_line("5-hour session", &u.five_hour));
                    item_week.set_text(limit_line("Weekly (all models)", &u.seven_day));
                    if u.seven_day_opus.is_some() {
                        item_opus.set_text(limit_line("Weekly (Opus)", &u.seven_day_opus));
                    } else if opus_present {
                        let _ = menu.remove(&item_opus);
                        opus_present = false;
                    }
                    item_updated.set_text(
                        chrono::Local::now()
                            .format("Updated %-I:%M %p")
                            .to_string(),
                    );
                }
                Err(e) => {
                    if let Some(t) = &tray {
                        t.set_title(Some("⚠️ Claude"));
                    }
                    item_updated.set_text(format!("Error: {e}"));
                }
            },

            Event::UserEvent(UserEvent::Menu(e)) => {
                if e.id == item_refresh.id() {
                    item_updated.set_text("Refreshing…");
                    let _ = poke_tx.send(());
                } else if e.id == item_open.id() {
                    let _ = Command::new("open")
                        .arg("https://claude.ai/settings/usage")
                        .spawn();
                } else if e.id == item_quit.id() {
                    tray.take();
                    *control_flow = ControlFlow::Exit;
                }
            }

            _ => {}
        }
    })
}
