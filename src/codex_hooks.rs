//! Passive OpenAI Codex lifecycle hooks for Now Bar. They are added to the user's
//! %USERPROFILE%\\.codex\\hooks.json and never make policy decisions. Codex's own /hooks
//! trust flow remains in control.

use std::path::PathBuf;

use serde_json::{json, Map, Value};

use crate::activities::live::{LiveMsg, LiveState};

/// Current Codex hook event names documented by Codex's hooks API.
pub const EVENTS: [&str; 14] = [
    "PreModelRequest", "PostModelRequest", "PreToolUse", "PermissionRequest", "PostToolUse",
    "PreCompact", "PostCompact", "SessionStart", "SessionEnd", "UserPromptSubmit",
    "SubagentStart", "SubagentStop", "Stop", "Interrupt",
];

fn command(exe: &str) -> String {
    format!("\\\"{}\\\" codex-hook", exe.replace('"', "\\\\\\""))
}

fn command_exe(command: &str) -> Option<&str> {
    let path = command.trim_start().strip_prefix('"')?.split_once('"')?.0;
    (!path.is_empty()).then_some(path)
}

/// Whether this handler belongs to Now Bar. Match the command token, not an app name.
pub fn is_ours(hook: &Value) -> bool {
    hook["type"].as_str() == Some("command")
        && hook["command"].as_str().is_some_and(|c| c.split_whitespace().last() == Some("codex-hook"))
}

fn our_hook(event: &str, exe: &str) -> Value {
    let mut handler = json!({"type": "command", "command": command(exe)});
    // SessionEnd is always synchronous in Codex. It is deliberately tiny and only sends a clear.
    if event != "SessionEnd" {
        handler["async"] = json!(true);
    }
    json!({"matcher": "", "hooks": [handler]})
}

/// Remove only our handlers. Unknown shapes and all third-party handlers stay as JSON values.
pub fn remove(mut settings: Value) -> Value {
    let Some(hooks) = settings.get_mut("hooks").and_then(Value::as_object_mut) else { return settings };
    for entries in hooks.values_mut() {
        let Some(list) = entries.as_array_mut() else { continue };
        list.retain_mut(|entry| {
            let Some(inner) = entry.get_mut("hooks").and_then(Value::as_array_mut) else { return true };
            let had = inner.len();
            inner.retain(|h| !is_ours(h));
            !(inner.is_empty() && had > 0)
        });
    }
    hooks.retain(|_, entries| entries.as_array().is_none_or(|l| !l.is_empty()));
    settings
}

/// Add one passive hook for each supported event. Returns None for an incompatible hooks shape.
pub fn install(settings: Value, exe: &str) -> Option<Value> {
    let mut settings = remove(settings);
    if !settings.is_object() {
        settings = Value::Object(Map::new());
    }
    let hooks = settings.as_object_mut()?.entry("hooks").or_insert_with(|| json!({})).as_object_mut()?;
    for event in EVENTS {
        let entries = hooks.entry(event).or_insert_with(|| json!([]));
        if let Some(list) = entries.as_array_mut() {
            list.push(our_hook(event, exe));
        } else {
            return None;
        }
    }
    Some(settings)
}

/// Repoint our handlers only if their old executable is gone.
pub fn follow(mut settings: Value, exe: &str, exists: impl Fn(&str) -> bool) -> Option<Value> {
    let ours: Vec<&mut Value> = settings.get_mut("hooks").and_then(Value::as_object_mut)?
        .values_mut().filter_map(Value::as_array_mut).flatten()
        .filter_map(|entry| entry.get_mut("hooks").and_then(Value::as_array_mut)).flatten()
        .filter(|h| is_ours(h)).collect();
    let commands: Vec<&str> = ours.iter().filter_map(|h| h["command"].as_str()).collect();
    let paths: Vec<&str> = commands.iter().filter_map(|c| command_exe(c)).collect();
    if paths.is_empty() || paths.iter().all(|c| *c == exe) || paths.iter().any(|c| *c != exe && exists(c)) {
        return None;
    }
    for h in ours {
        h["command"] = json!(command(exe));
    }
    Some(settings)
}

pub fn read_settings(path: &std::path::Path) -> std::io::Result<String> {
    match std::fs::read_to_string(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok("{}".into()),
        other => other,
    }
}

/// Codex's user-level hook file. Project-level hooks are intentionally not modified.
pub fn settings_path() -> Option<PathBuf> {
    let home = std::env::var_os("USERPROFILE").map(PathBuf::from)?;
    Some(home.join(".codex").join("hooks.json"))
}

pub fn rewrite(change: impl FnOnce(Value) -> Option<Value>) -> std::io::Result<bool> {
    let path = settings_path().ok_or_else(|| std::io::Error::other("no user profile"))?;
    rewrite_at(&path, change)
}

pub fn rewrite_at(path: &std::path::Path, change: impl FnOnce(Value) -> Option<Value>) -> std::io::Result<bool> {
    let text = read_settings(path)?;
    let current: Value = serde_json::from_str(text.trim_start_matches('\\u{feff}')).map_err(std::io::Error::other)?;
    let Some(next) = change(current) else { return Ok(false) };
    if let Some(dir) = path.parent() { std::fs::create_dir_all(dir)?; }
    let backup = path.with_extension("json.before-nowbar");
    if path.exists() {
        std::fs::copy(&path, if backup.exists() { path.with_extension("json.nowbar-last") } else { backup })?;
    }
    let pretty = serde_json::to_string_pretty(&next).map_err(std::io::Error::other)?;
    let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    std::fs::write(&tmp, if text.contains("\\r\\n") { pretty.replace('\\n', "\\r\\n") } else { pretty })?;
    std::fs::rename(&tmp, path)?;
    Ok(true)
}

/// Convert a Codex event to a live item. Prompt text, transcript contents and tool arguments
/// are intentionally never copied into the activity.
pub fn message(hook: &Value) -> Option<LiveMsg> {
    let event = hook["hook_event_name"].as_str()?;
    let session = hook["session_id"].as_str()?;
    if session.is_empty() { return None; }
    let tool = hook["tool_name"].as_str().filter(|s| !s.is_empty());
    let (state, text) = match event {
        "SessionStart" => (LiveState::Run, "Session started".to_string()),
        "UserPromptSubmit" => (LiveState::Run, "Working on your prompt".to_string()),
        "PreModelRequest" => (LiveState::Run, "Thinking".to_string()),
        "PostModelRequest" => (LiveState::Run, "Model response received".to_string()),
        "PreToolUse" => (LiveState::Run, format!("Running {}", tool.unwrap_or("tool"))),
        "PostToolUse" => (LiveState::Run, format!("Finished {}", tool.unwrap_or("tool"))),
        "PermissionRequest" => (LiveState::Wait, format!("Waiting for permission · {}", tool.unwrap_or("tool"))),
        "PreCompact" => (LiveState::Run, "Compacting context".to_string()),
        "PostCompact" => (LiveState::Run, "Context compacted".to_string()),
        "SubagentStart" => (LiveState::Run, "Subagent started".to_string()),
        "SubagentStop" => (LiveState::Run, "Subagent finished".to_string()),
        "Stop" => (LiveState::Done, "Turn completed".to_string()),
        "Interrupt" => (LiveState::Done, "Interrupted".to_string()),
        "SessionEnd" => (LiveState::Clear, String::new()),
        _ => return None,
    };
    Some(LiveMsg { id: format!("codex:{session}"), title: "Codex".into(), text, progress: None, state, claude: None, app: None })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const EXE: &str = r"C:\Apps\NowBar\nowbar.exe";

    fn ours(v: &Value, event: &str) -> Vec<Value> {
        v["hooks"][event].as_array().into_iter().flatten()
            .flat_map(|e| e["hooks"].as_array().cloned().unwrap_or_default())
            .filter(is_ours).collect()
    }

    #[test]
    fn install_adds_supported_events_as_passive_commands() {
        let v = install(json!({}), EXE).unwrap();
        for event in EVENTS {
            let handlers = ours(&v, event);
            assert_eq!(handlers.len(), 1, "{event}");
            assert_eq!(handlers[0]["type"], "command");
            assert_eq!(handlers[0]["command"], command(EXE));
            assert_eq!(handlers[0]["async"], event != "SessionEnd");
        }
    }

    #[test]
    fn install_is_idempotent_and_preserves_other_hooks() {
        let other = json!({"type":"command","command":"other-hook.cmd"});
        let before = json!({"model":"gpt-5","hooks":{"Stop":[{"hooks":[other.clone()]}]}});
        let twice = install(install(before, r"C:\old\nowbar.exe").unwrap(), EXE).unwrap();
        assert_eq!(ours(&twice, "Stop").len(), 1);
        assert!(twice["hooks"]["Stop"].to_string().contains("other-hook.cmd"));
        assert_eq!(twice["model"], "gpt-5");
        let removed = remove(twice);
        assert_eq!(ours(&removed, "Stop").len(), 0);
        assert!(removed["hooks"]["Stop"].to_string().contains("other-hook.cmd"));
    }

    #[test]
    fn incompatible_hooks_shape_is_not_overwritten() {
        assert!(install(json!({"hooks":"disabled"}), EXE).is_none());
    }

    #[test]
    fn follow_only_repoints_missing_executable() {
        let old = install(json!({}), r"C:\old\nowbar.exe").unwrap();
        assert!(follow(old.clone(), EXE, |_| true).is_none());
        let moved = follow(old, EXE, |_| false).unwrap();
        assert_eq!(ours(&moved, "Stop")[0]["command"], command(EXE));
    }

    #[test]
    fn event_messages_never_copy_prompts_or_tool_arguments() {
        let hook = json!({"hook_event_name":"PreToolUse","session_id":"thread-123","tool_name":"Bash","tool_input":{"command":"echo secret"},"prompt":"do not show this"});
        let msg = message(&hook).unwrap();
        assert_eq!(msg.id, "codex:thread-123");
        assert_eq!(msg.title, "Codex");
        assert_eq!(msg.text, "Running Bash");
        assert!(!msg.text.contains("secret"));
    }

    #[test]
    fn session_end_clears_and_unknown_events_are_ignored() {
        assert_eq!(message(&json!({"hook_event_name":"SessionEnd","session_id":"s"})).unwrap().state, LiveState::Clear);
        assert!(message(&json!({"hook_event_name":"Unknown","session_id":"s"})).is_none());
    }
}
