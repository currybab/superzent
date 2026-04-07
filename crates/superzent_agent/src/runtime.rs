use anyhow::{Context, Result, anyhow, bail};
use collections::HashMap;
use serde::Deserialize;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    thread,
};
use superzent_model::{AgentPreset, AgentSession, PresetLaunchMode, WorkspaceEntry};
use task::{HideStrategy, RevealStrategy, RevealTarget, Shell, SpawnInTerminal, TaskId};
use tiny_http::{Response, Server};
use url::Url;
use uuid::Uuid;

pub const AGENT_HOOK_VERSION: &str = "1";
pub const AGENT_HOOK_URL_ENV_VAR: &str = "SUPERZENT_AGENT_HOOK_URL";
pub const AGENT_HOOK_VERSION_ENV_VAR: &str = "SUPERZENT_HOOK_VERSION";
pub const AGENT_REAL_CLAUDE_BIN_ENV_VAR: &str = "SUPERZENT_REAL_CLAUDE_BIN";
pub const AGENT_REAL_CODEX_BIN_ENV_VAR: &str = "SUPERZENT_REAL_CODEX_BIN";
pub const AGENT_HOOK_BIN_DIR_ENV_VAR: &str = "SUPERZENT_AGENT_HOOK_BIN_DIR";
pub const AGENT_TERMINAL_ID_ENV_VAR: &str = "SUPERZENT_TERMINAL_ID";
pub const AGENT_WORKSPACE_ID_ENV_VAR: &str = "SUPERZENT_WORKSPACE_ID";
pub const AGENT_DEBUG_HOOKS_ENV_VAR: &str = "SUPERZENT_DEBUG_HOOKS";

const HOOK_ENDPOINT_PATH: &str = "/agent-hook";
const NOTIFY_SCRIPT_FILE_NAME: &str = "notify.sh";
const WRAPPER_MARKER: &str = "# Superzent agent wrapper v1";

static HOOK_RUNTIME: OnceLock<AgentHookRuntime> = OnceLock::new();

fn debug_hooks_enabled() -> bool {
    std::env::var("SUPERZENT_DEBUG_HOOKS")
        .map(|value| !matches!(value.as_str(), "0" | "false" | "FALSE" | "False"))
        .unwrap_or(false)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedWorkspaceLaunch {
    pub command: String,
    pub args: Vec<String>,
    pub environment: HashMap<String, String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AgentHookEventType {
    Start,
    Stop,
    PermissionRequest,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentHookEvent {
    pub event_type: AgentHookEventType,
    pub terminal_id: String,
    pub workspace_id: Option<String>,
    pub session_id: Option<String>,
    pub cwd: Option<PathBuf>,
}

#[derive(Clone, Debug)]
pub struct AgentHookPaths {
    pub bin_dir: PathBuf,
    pub hook_url: String,
}

pub fn subscribe() -> Result<smol::channel::Receiver<AgentHookEvent>> {
    let runtime = runtime()?;
    let (sender, receiver) = smol::channel::unbounded();
    runtime
        .subscribers
        .lock()
        .map_err(|_| anyhow!("failed to lock agent hook subscribers"))?
        .push(sender);
    Ok(receiver)
}

pub fn inject_terminal_environment(environment: &mut HashMap<String, String>) -> Result<String> {
    let runtime = runtime()?;
    let terminal_id = Uuid::new_v4().to_string();

    environment.insert(
        AGENT_HOOK_URL_ENV_VAR.to_string(),
        runtime.paths.hook_url.clone(),
    );
    environment.insert(
        AGENT_HOOK_VERSION_ENV_VAR.to_string(),
        AGENT_HOOK_VERSION.to_string(),
    );
    environment.insert(
        AGENT_HOOK_BIN_DIR_ENV_VAR.to_string(),
        runtime.paths.bin_dir.to_string_lossy().to_string(),
    );
    environment.insert(AGENT_TERMINAL_ID_ENV_VAR.to_string(), terminal_id.clone());
    if let Ok(debug_hooks) = std::env::var(AGENT_DEBUG_HOOKS_ENV_VAR) {
        environment.insert(AGENT_DEBUG_HOOKS_ENV_VAR.to_string(), debug_hooks);
    }
    prepend_path_entry(environment, &runtime.paths.bin_dir);

    Ok(terminal_id)
}

pub fn spawn_for_workspace(
    workspace: &WorkspaceEntry,
    session: &AgentSession,
    preset: &AgentPreset,
) -> Result<SpawnInTerminal> {
    let (label, full_label) = terminal_tab_labels(workspace, preset);
    let command_label = if preset.args.is_empty() {
        preset.command.clone()
    } else {
        format!("{} {}", preset.command, preset.args.join(" "))
    };
    let launch = prepare_workspace_launch(workspace, preset)?;

    Ok(SpawnInTerminal {
        id: TaskId(format!("superzent:{}:{}", workspace.id, session.id)),
        full_label,
        label,
        command: Some(launch.command),
        args: launch.args,
        command_label,
        cwd: Some(workspace.cwd_path()),
        env: launch.environment,
        use_new_terminal: true,
        allow_concurrent_runs: true,
        reveal: RevealStrategy::Always,
        reveal_target: RevealTarget::Center,
        hide: HideStrategy::Never,
        shell: Shell::System,
        show_summary: true,
        show_command: true,
        show_rerun: true,
    })
}

pub fn prepare_workspace_launch(
    workspace: &WorkspaceEntry,
    preset: &AgentPreset,
) -> Result<PreparedWorkspaceLaunch> {
    if preset.launch_mode != PresetLaunchMode::Terminal {
        bail!("ACP presets cannot be launched in a terminal");
    }

    let mut environment = preset.env.clone().into_iter().collect::<HashMap<_, _>>();
    inject_terminal_environment(&mut environment)?;
    environment.insert(AGENT_WORKSPACE_ID_ENV_VAR.to_string(), workspace.id.clone());

    let managed_command = ManagedCommand::for_command(&preset.command);
    let (command, args) = if let Some(managed_command) = managed_command {
        environment.insert(
            managed_command.real_binary_env_var().to_string(),
            preset.command.clone(),
        );
        (
            managed_command.binary_name().to_string(),
            preset.args.clone(),
        )
    } else {
        (preset.command.clone(), preset.args.clone())
    };

    Ok(PreparedWorkspaceLaunch {
        command,
        args,
        environment,
    })
}

fn terminal_tab_labels(workspace: &WorkspaceEntry, preset: &AgentPreset) -> (String, String) {
    (
        preset.label.clone(),
        format!("{} · {}", workspace.name, preset.label),
    )
}

fn runtime() -> Result<&'static AgentHookRuntime> {
    if let Some(runtime) = HOOK_RUNTIME.get() {
        return Ok(runtime);
    }

    let runtime = AgentHookRuntime::new()?;
    let _ = HOOK_RUNTIME.set(runtime);
    HOOK_RUNTIME
        .get()
        .context("failed to initialize agent hook runtime")
}

struct AgentHookRuntime {
    paths: AgentHookPaths,
    subscribers: Arc<Mutex<Vec<smol::channel::Sender<AgentHookEvent>>>>,
}

impl AgentHookRuntime {
    fn new() -> Result<Self> {
        let bin_dir = ensure_agent_hook_wrapper_files()?;

        let server = Server::http("127.0.0.1:0")
            .map_err(|error| anyhow!(error).context("bind hook port"))?;
        let hook_url = format!(
            "http://127.0.0.1:{}{}",
            server.server_addr().port(),
            HOOK_ENDPOINT_PATH
        );

        let subscribers = Arc::new(Mutex::new(Vec::new()));
        spawn_hook_server(server, subscribers.clone());

        Ok(Self {
            paths: AgentHookPaths { bin_dir, hook_url },
            subscribers,
        })
    }
}

fn ensure_agent_hook_wrapper_files() -> Result<PathBuf> {
    let root_dir = paths::data_dir().join("agent-hooks");
    let bin_dir = root_dir.join("bin");
    let hooks_dir = root_dir.join("hooks");
    fs::create_dir_all(&bin_dir)?;
    fs::create_dir_all(&hooks_dir)?;

    let notify_script_path = hooks_dir.join(NOTIFY_SCRIPT_FILE_NAME);

    write_executable_file(&notify_script_path, notify_script_content())?;
    write_executable_file(
        &bin_dir.join("claude"),
        claude_wrapper_content(&bin_dir, &notify_script_path)?,
    )?;
    write_executable_file(
        &bin_dir.join("codex"),
        codex_wrapper_content(&bin_dir, &notify_script_path),
    )?;

    Ok(bin_dir)
}

fn spawn_hook_server(
    server: Server,
    subscribers: Arc<Mutex<Vec<smol::channel::Sender<AgentHookEvent>>>>,
) {
    thread::Builder::new()
        .name("superzent-agent-hooks".to_string())
        .spawn(move || {
            loop {
                let Ok(request) = server.recv() else {
                    break;
                };

                let response = match parse_request(request.url()) {
                    Ok(Some(event)) => {
                        if debug_hooks_enabled() {
                            log::info!(
                                "superzent hook server accepted event: type={:?} terminal_id={} workspace_id={:?} session_id={:?} cwd={:?}",
                                event.event_type,
                                event.terminal_id,
                                event.workspace_id,
                                event.session_id,
                                event.cwd,
                            );
                        }
                        if let Ok(mut subscribers) = subscribers.lock() {
                            subscribers
                                .retain(|sender| sender.send_blocking(event.clone()).is_ok());
                        }
                        Response::empty(204)
                    }
                    Ok(None) => {
                        if debug_hooks_enabled() {
                            log::info!("superzent hook server ignored request: url={}", request.url());
                        }
                        Response::empty(204)
                    }
                    Err(error) => {
                        log::warn!("failed to parse agent hook request: {error:#}");
                        Response::empty(400)
                    }
                };

                if let Err(error) = request.respond(response) {
                    log::debug!("failed to respond to agent hook request: {error}");
                }
            }
        })
        .ok();
}

fn parse_request(url: &str) -> Result<Option<AgentHookEvent>> {
    let url =
        Url::parse(&format!("http://127.0.0.1{url}")).context("failed to parse agent hook url")?;
    if url.path() != HOOK_ENDPOINT_PATH {
        if debug_hooks_enabled() {
            log::info!("superzent hook parse skipped non-hook path: {}", url.path());
        }
        return Ok(None);
    }

    let query = url.query().unwrap_or_default();
    let params: HookRequestParams =
        serde_urlencoded::from_str(query).context("failed to parse hook query parameters")?;

    if let Some(version) = params.version.as_deref()
        && version != AGENT_HOOK_VERSION
    {
        log::warn!("ignoring agent hook event with unsupported version `{version}`");
        return Ok(None);
    }

    let Some(event_type) = params.event_type.as_deref().and_then(map_hook_event_type) else {
        if debug_hooks_enabled() {
            log::info!(
                "superzent hook parse ignored unknown event_type: raw={:?} query={}",
                params.event_type,
                query
            );
        }
        return Ok(None);
    };

    let terminal_id = params
        .terminal_id
        .filter(|terminal_id| !terminal_id.trim().is_empty())
        .context("missing terminal_id in agent hook request")?;

    if debug_hooks_enabled() {
        log::info!(
            "superzent hook parse mapped event: raw={:?} mapped={:?} terminal_id={} workspace_id={:?} session_id={:?}",
            params.event_type,
            event_type,
            terminal_id,
            params.workspace_id,
            params.session_id,
        );
    }

    Ok(Some(AgentHookEvent {
        event_type,
        terminal_id,
        workspace_id: params
            .workspace_id
            .filter(|workspace_id| !workspace_id.trim().is_empty()),
        session_id: params
            .session_id
            .filter(|session_id| !session_id.trim().is_empty()),
        cwd: params.cwd.map(PathBuf::from),
    }))
}

#[derive(Debug, Deserialize)]
struct HookRequestParams {
    #[serde(rename = "cwd")]
    cwd: Option<String>,
    #[serde(rename = "event_type")]
    event_type: Option<String>,
    #[serde(rename = "session_id")]
    session_id: Option<String>,
    #[serde(rename = "terminal_id")]
    terminal_id: Option<String>,
    #[serde(rename = "version")]
    version: Option<String>,
    #[serde(rename = "workspace_id")]
    workspace_id: Option<String>,
}

fn map_hook_event_type(event_type: &str) -> Option<AgentHookEventType> {
    match event_type {
        "Start"
        | "UserPromptSubmit"
        | "PostToolUse"
        | "PostToolUseFailure"
        | "BeforeAgent"
        | "AfterTool"
        | "SessionStart"
        | "sessionStart"
        | "userPromptSubmitted"
        | "postToolUse" => Some(AgentHookEventType::Start),
        "PermissionRequest" | "preToolUse" | "Notification" => {
            Some(AgentHookEventType::PermissionRequest)
        }
        "Stop" | "AfterAgent" | "agent-turn-complete" | "sessionEnd" => {
            Some(AgentHookEventType::Stop)
        }
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ManagedCommand {
    Claude,
    Codex,
}

impl ManagedCommand {
    fn for_command(command: &str) -> Option<Self> {
        let file_name = Path::new(command)
            .file_name()?
            .to_str()?
            .to_ascii_lowercase();
        match file_name.as_str() {
            "claude" | "claude.exe" => Some(Self::Claude),
            "codex" | "codex.exe" => Some(Self::Codex),
            _ => None,
        }
    }

    fn binary_name(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }

    fn real_binary_env_var(self) -> &'static str {
        match self {
            Self::Claude => AGENT_REAL_CLAUDE_BIN_ENV_VAR,
            Self::Codex => AGENT_REAL_CODEX_BIN_ENV_VAR,
        }
    }
}

fn prepend_path_entry(environment: &mut HashMap<String, String>, path: &Path) {
    let path = path.to_string_lossy().to_string();
    let existing_path = environment
        .get("PATH")
        .cloned()
        .filter(|existing_path| !existing_path.is_empty())
        .or_else(|| std::env::var("PATH").ok())
        .unwrap_or_default();

    if existing_path.is_empty() {
        environment.insert("PATH".to_string(), path);
    } else {
        environment.insert("PATH".to_string(), format!("{path}:{existing_path}"));
    }
}

fn write_executable_file(path: &Path, contents: String) -> Result<()> {
    fs::write(path, contents)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = fs::metadata(path)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions)?;
    }

    Ok(())
}

fn notify_script_content() -> String {
    r#"#!/bin/bash
# Superzent agent notification hook

_superzent_debug_enabled=0
case "${SUPERZENT_DEBUG_HOOKS:-}" in
  1|true|TRUE|True|yes|YES|on|ON) _superzent_debug_enabled=1 ;;
esac
_superzent_debug_log="${TMPDIR:-/tmp}/superzent-notify-debug.log"

if [ -n "$1" ]; then
  INPUT="$1"
else
  INPUT=$(cat)
fi

[ "$_superzent_debug_enabled" = "1" ] && echo "$(date '+%H:%M:%S') notify.sh called hook_url=$SUPERZENT_AGENT_HOOK_URL terminal_id=$SUPERZENT_TERMINAL_ID workspace_id=$SUPERZENT_WORKSPACE_ID input=$INPUT" >> "$_superzent_debug_log"

if [ -z "$SUPERZENT_AGENT_HOOK_URL" ] || [ -z "$SUPERZENT_TERMINAL_ID" ]; then
  [ "$_superzent_debug_enabled" = "1" ] && echo "$(date '+%H:%M:%S') notify.sh skipped missing hook env" >> "$_superzent_debug_log"
  exit 0
fi

EVENT_TYPE=$(printf '%s\n' "$INPUT" | grep -oE '"hook_event_name"[[:space:]]*:[[:space:]]*"[^"]*"' | grep -oE '"[^"]*"$' | tr -d '"')
if [ -z "$EVENT_TYPE" ]; then
  EVENT_TYPE=$(printf '%s\n' "$INPUT" | grep -oE '"type"[[:space:]]*:[[:space:]]*"[^"]*"' | grep -oE '"[^"]*"$' | tr -d '"')
fi

if [ -z "$EVENT_TYPE" ]; then
  [ "$_superzent_debug_enabled" = "1" ] && echo "$(date '+%H:%M:%S') notify.sh skipped missing event_type" >> "$_superzent_debug_log"
  exit 0
fi

_superzent_status=$(curl -sSG "$SUPERZENT_AGENT_HOOK_URL" \
  --connect-timeout 1 \
  --max-time 2 \
  --data-urlencode "event_type=$EVENT_TYPE" \
  --data-urlencode "terminal_id=$SUPERZENT_TERMINAL_ID" \
  --data-urlencode "workspace_id=$SUPERZENT_WORKSPACE_ID" \
  --data-urlencode "session_id=$SUPERZENT_SESSION_ID" \
  --data-urlencode "cwd=$PWD" \
  --data-urlencode "version=$SUPERZENT_HOOK_VERSION" \
  -o /dev/null -w "%{http_code}" 2>/dev/null)
_superzent_exit=$?
[ "$_superzent_debug_enabled" = "1" ] && echo "$(date '+%H:%M:%S') notify.sh dispatched event_type=$EVENT_TYPE curl_exit=$_superzent_exit status=$_superzent_status" >> "$_superzent_debug_log"

exit 0
"#
    .to_string()
}

fn claude_settings_content(notify_script_path: &Path) -> Result<String> {
    let notify_script_path = notify_script_path.to_string_lossy().to_string();
    let notify_command = format!(
        "[ -x {path} ] && {path} || true",
        path = shell_single_quote(&notify_script_path)
    );
    let settings = serde_json::json!({
        "hooks": {
            "UserPromptSubmit": [{ "hooks": [{ "type": "command", "command": notify_command }] }],
            "SessionStart": [{ "hooks": [{ "type": "command", "command": notify_command }] }],
            "Stop": [{ "hooks": [{ "type": "command", "command": notify_command }] }],
            "PostToolUse": [{ "matcher": "*", "hooks": [{ "type": "command", "command": notify_command }] }],
            "PostToolUseFailure": [{ "matcher": "*", "hooks": [{ "type": "command", "command": notify_command }] }],
            "PermissionRequest": [{ "matcher": "*", "hooks": [{ "type": "command", "command": notify_command }] }],
        }
    });
    serde_json::to_string(&settings).context("failed to serialize Claude settings")
}

fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn wrapper_resolver_content(binary_name: &str, override_env_var: &str, bin_dir: &Path) -> String {
    let bin_dir = bin_dir.to_string_lossy();
    format!(
        r#"find_real_binary() {{
  local override="${{{override_env_var}:-}}"
  local name="{binary_name}"

  if [ -n "$override" ] && [ -x "$override" ] && [ ! -d "$override" ]; then
    printf "%s\n" "$override"
    return 0
  fi

  local IFS=:
  for dir in $PATH; do
    [ -z "$dir" ] && continue
    dir="${{dir%/}}"
    case "$dir" in
      "{bin_dir}") continue ;;
    esac
    if [ -x "$dir/$name" ] && [ ! -d "$dir/$name" ]; then
      printf "%s\n" "$dir/$name"
      return 0
    fi
  done
  return 1
}}
"#
    )
}

fn claude_wrapper_content(bin_dir: &Path, notify_script_path: &Path) -> Result<String> {
    let claude_settings_json = shell_single_quote(&claude_settings_content(notify_script_path)?);
    Ok(format!(
        r#"#!/bin/bash
{WRAPPER_MARKER}
_superzent_debug_enabled=0
case "${{{AGENT_DEBUG_HOOKS_ENV_VAR}:-}}" in
  1|true|TRUE|True|yes|YES|on|ON) _superzent_debug_enabled=1 ;;
esac
_superzent_debug_log="${{TMPDIR:-/tmp}}/superzent-notify-debug.log"
if [ "$_superzent_debug_enabled" = "1" ]; then
  echo "$(date '+%H:%M:%S') claude wrapper invoked hook_url=$SUPERZENT_AGENT_HOOK_URL terminal_id=$SUPERZENT_TERMINAL_ID" >> "$_superzent_debug_log"
fi
{resolver}
REAL_BIN="$(find_real_binary)"
if [ -z "$REAL_BIN" ]; then
  echo "Superzent: claude not found in PATH." >&2
  [ "$_superzent_debug_enabled" = "1" ] && echo "$(date '+%H:%M:%S') claude wrapper failed: real binary not found" >> "$_superzent_debug_log"
  exit 127
fi

if [ "$_superzent_debug_enabled" = "1" ]; then
  echo "$(date '+%H:%M:%S') claude wrapper exec REAL_BIN=$REAL_BIN" >> "$_superzent_debug_log"
fi
exec "$REAL_BIN" --settings {claude_settings_json} "$@"
"#,
        resolver = wrapper_resolver_content("claude", AGENT_REAL_CLAUDE_BIN_ENV_VAR, bin_dir),
    ))
}

fn codex_wrapper_content(bin_dir: &Path, notify_script_path: &Path) -> String {
    let notify_script_path = notify_script_path.to_string_lossy();
    format!(
        r#"#!/bin/bash
{WRAPPER_MARKER}
{resolver}
REAL_BIN="$(find_real_binary)"
if [ -z "$REAL_BIN" ]; then
  echo "Superzent: codex not found in PATH." >&2
  exit 127
fi

if [ -n "$SUPERZENT_TERMINAL_ID" ] && [ -f "{notify_script_path}" ]; then
  export CODEX_TUI_RECORD_SESSION=1
  if [ -z "$CODEX_TUI_SESSION_LOG_PATH" ]; then
    _superzent_codex_ts="$(date +%s 2>/dev/null || echo "$$")"
    export CODEX_TUI_SESSION_LOG_PATH="${{TMPDIR:-/tmp}}/superzent-codex-session-$$_${{_superzent_codex_ts}}.jsonl"
  fi

  (
    _superzent_log="$CODEX_TUI_SESSION_LOG_PATH"
    _superzent_notify="{notify_script_path}"
    _superzent_last_turn_id=""
    _superzent_last_approval_id=""
    _superzent_last_exec_call_id=""
    _superzent_approval_fallback_seq=0

    _superzent_emit_event() {{
      _superzent_event="$1"
      bash "$_superzent_notify" "$(printf '{{"hook_event_name":"%s"}}' "$_superzent_event")" >/dev/null 2>&1 || true
    }}

    _superzent_i=0
    while [ ! -f "$_superzent_log" ] && [ "$_superzent_i" -lt 200 ]; do
      _superzent_i=$((_superzent_i + 1))
      sleep 0.05
    done
    if [ ! -f "$_superzent_log" ]; then
      exit 0
    fi

    tail -n 0 -F "$_superzent_log" 2>/dev/null | while IFS= read -r _superzent_line; do
      case "$_superzent_line" in
        *'"dir":"to_tui"'*'"kind":"codex_event"'*'"msg":{{"type":"task_started"'*)
          _superzent_turn_id=$(printf '%s\n' "$_superzent_line" | awk -F'"turn_id":"' 'NF > 1 {{ sub(/".*/, "", $2); print $2; exit }}')
          [ -n "$_superzent_turn_id" ] || _superzent_turn_id="task_started"
          if [ "$_superzent_turn_id" != "$_superzent_last_turn_id" ]; then
            _superzent_last_turn_id="$_superzent_turn_id"
            _superzent_emit_event "Start"
          fi
          ;;
        *'"dir":"to_tui"'*'"kind":"codex_event"'*'"msg":{{"type":"'*'_approval_request"'*)
          _superzent_approval_id=$(printf '%s\n' "$_superzent_line" | awk -F'"id":"' 'NF > 1 {{ sub(/".*/, "", $2); print $2; exit }}')
          [ -n "$_superzent_approval_id" ] || _superzent_approval_id=$(printf '%s\n' "$_superzent_line" | awk -F'"approval_id":"' 'NF > 1 {{ sub(/".*/, "", $2); print $2; exit }}')
          [ -n "$_superzent_approval_id" ] || _superzent_approval_id=$(printf '%s\n' "$_superzent_line" | awk -F'"call_id":"' 'NF > 1 {{ sub(/".*/, "", $2); print $2; exit }}')
          if [ -z "$_superzent_approval_id" ]; then
            _superzent_approval_fallback_seq=$((_superzent_approval_fallback_seq + 1))
            _superzent_approval_id="approval_request_${{_superzent_approval_fallback_seq}}"
          fi
          if [ "$_superzent_approval_id" != "$_superzent_last_approval_id" ]; then
            _superzent_last_approval_id="$_superzent_approval_id"
            _superzent_emit_event "PermissionRequest"
          fi
          ;;
        *'"dir":"to_tui"'*'"kind":"codex_event"'*'"msg":{{"type":"exec_command_begin"'*)
          _superzent_exec_call_id=$(printf '%s\n' "$_superzent_line" | awk -F'"call_id":"' 'NF > 1 {{ sub(/".*/, "", $2); print $2; exit }}')
          if [ -n "$_superzent_exec_call_id" ]; then
            if [ "$_superzent_exec_call_id" != "$_superzent_last_exec_call_id" ]; then
              _superzent_last_exec_call_id="$_superzent_exec_call_id"
              _superzent_emit_event "Start"
            fi
          else
            _superzent_emit_event "Start"
          fi
          ;;
      esac
    done
  ) &
  SUPERZENT_CODEX_START_WATCHER_PID=$!
fi

"$REAL_BIN" -c "notify=[\"bash\",\"{notify_script_path}\"]" "$@"
SUPERZENT_CODEX_STATUS=$?

if [ -n "$SUPERZENT_CODEX_START_WATCHER_PID" ]; then
  kill "$SUPERZENT_CODEX_START_WATCHER_PID" >/dev/null 2>&1 || true
  wait "$SUPERZENT_CODEX_START_WATCHER_PID" 2>/dev/null || true
fi

exit "$SUPERZENT_CODEX_STATUS"
"#,
        resolver = wrapper_resolver_content("codex", AGENT_REAL_CODEX_BIN_ENV_VAR, bin_dir),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use superzent_model::{
        WorkspaceAttentionStatus, WorkspaceGitStatus, WorkspaceKind, WorkspaceLocation,
    };

    #[test]
    fn maps_supported_event_types() {
        assert_eq!(
            map_hook_event_type("Start"),
            Some(AgentHookEventType::Start)
        );
        assert_eq!(
            map_hook_event_type("UserPromptSubmit"),
            Some(AgentHookEventType::Start)
        );
        assert_eq!(
            map_hook_event_type("PermissionRequest"),
            Some(AgentHookEventType::PermissionRequest)
        );
        assert_eq!(
            map_hook_event_type("Notification"),
            Some(AgentHookEventType::PermissionRequest)
        );
        assert_eq!(
            map_hook_event_type("sessionStart"),
            Some(AgentHookEventType::Start)
        );
        assert_eq!(
            map_hook_event_type("userPromptSubmitted"),
            Some(AgentHookEventType::Start)
        );
        assert_eq!(
            map_hook_event_type("postToolUse"),
            Some(AgentHookEventType::Start)
        );
        assert_eq!(
            map_hook_event_type("agent-turn-complete"),
            Some(AgentHookEventType::Stop)
        );
        assert_eq!(
            map_hook_event_type("sessionEnd"),
            Some(AgentHookEventType::Stop)
        );
        assert_eq!(map_hook_event_type("Unknown"), None);
    }

    #[test]
    fn parses_valid_hook_request() {
        let event = parse_request(
            "/agent-hook?event_type=Stop&terminal_id=terminal-1&workspace_id=workspace-1&cwd=%2Ftmp%2Fproject&version=1",
        )
        .expect("request should parse")
        .expect("request should produce an event");

        assert_eq!(event.event_type, AgentHookEventType::Stop);
        assert_eq!(event.terminal_id, "terminal-1");
        assert_eq!(event.workspace_id.as_deref(), Some("workspace-1"));
        assert_eq!(event.cwd.as_deref(), Some(Path::new("/tmp/project")));
    }

    #[test]
    fn ignores_version_mismatches() {
        let event = parse_request("/agent-hook?event_type=Stop&terminal_id=terminal-1&version=999")
            .expect("request should parse");

        assert_eq!(event, None);
    }

    #[test]
    fn wrapper_prefers_override_binary_paths() {
        let wrapper =
            claude_wrapper_content(Path::new("/tmp/bin"), Path::new("/tmp/hooks/notify.sh"))
                .expect("Claude wrapper should render");
        assert!(wrapper.contains(AGENT_REAL_CLAUDE_BIN_ENV_VAR));
        let settings_arg_prefix = "--settings '";
        let settings_start = wrapper
            .find(settings_arg_prefix)
            .expect("Claude wrapper should include inline settings")
            + settings_arg_prefix.len();
        let settings_end = wrapper[settings_start..]
            .find("' \"$@\"")
            .expect("Claude wrapper should terminate inline settings")
            + settings_start;
        let encoded_settings = &wrapper[settings_start..settings_end];
        let decoded_settings = encoded_settings.replace("'\"'\"'", "'");
        let settings: serde_json::Value =
            serde_json::from_str(&decoded_settings).expect("inline settings should be valid JSON");
        assert!(settings.get("hooks").is_some());
        assert_eq!(
            settings["hooks"]["SessionStart"][0]["hooks"][0]["type"],
            "command"
        );

        let wrapper =
            codex_wrapper_content(Path::new("/tmp/bin"), Path::new("/tmp/hooks/notify.sh"));
        assert!(wrapper.contains(AGENT_REAL_CODEX_BIN_ENV_VAR));
        assert!(wrapper.contains("notify=[\\\"bash\\\",\\\"/tmp/hooks/notify.sh\\\"]"));
    }

    #[test]
    fn terminal_tab_uses_preset_label_and_keeps_workspace_in_full_label() {
        let workspace = WorkspaceEntry {
            id: "workspace-1".to_string(),
            project_id: "project-1".to_string(),
            kind: WorkspaceKind::Worktree,
            name: "feature-branch".to_string(),
            display_name: None,
            branch: "feature-branch".to_string(),
            location: WorkspaceLocation::Local {
                worktree_path: PathBuf::from("/tmp/feature-branch"),
            },
            agent_preset_id: "codex".to_string(),
            managed: true,
            git_status: WorkspaceGitStatus::Available,
            git_summary: None,
            attention_status: WorkspaceAttentionStatus::Idle,
            review_pending: false,
            last_attention_reason: None,
            created_at: Default::default(),
            last_opened_at: Default::default(),
        };
        let preset = AgentPreset {
            id: "codex".to_string(),
            label: "Codex".to_string(),
            launch_mode: PresetLaunchMode::Terminal,
            command: "codex".to_string(),
            args: Vec::new(),
            env: Default::default(),
            acp_agent_name: Some("codex-acp".to_string()),
            attention_patterns: Vec::new(),
        };

        let (label, full_label) = terminal_tab_labels(&workspace, &preset);

        assert_eq!(label, "Codex");
        assert_eq!(full_label, "feature-branch · Codex");
    }

    #[test]
    fn prepare_workspace_launch_rejects_acp_presets() {
        let workspace = WorkspaceEntry {
            id: "workspace-1".to_string(),
            project_id: "project-1".to_string(),
            kind: WorkspaceKind::Worktree,
            name: "feature-branch".to_string(),
            display_name: None,
            branch: "feature-branch".to_string(),
            location: WorkspaceLocation::Local {
                worktree_path: PathBuf::from("/tmp/feature-branch"),
            },
            agent_preset_id: "codex".to_string(),
            managed: true,
            git_status: WorkspaceGitStatus::Available,
            git_summary: None,
            attention_status: WorkspaceAttentionStatus::Idle,
            review_pending: false,
            last_attention_reason: None,
            created_at: Default::default(),
            last_opened_at: Default::default(),
        };
        let preset = AgentPreset {
            id: "codex".to_string(),
            label: "Codex".to_string(),
            launch_mode: PresetLaunchMode::Acp,
            command: "codex".to_string(),
            args: Vec::new(),
            env: Default::default(),
            acp_agent_name: Some("codex-acp".to_string()),
            attention_patterns: Vec::new(),
        };

        let error = prepare_workspace_launch(&workspace, &preset)
            .expect_err("ACP presets should not use the terminal launch path");
        assert_eq!(
            error.to_string(),
            "ACP presets cannot be launched in a terminal"
        );
    }

    #[test]
    fn prepare_workspace_launch_uses_managed_command_name_for_managed_commands() {
        let workspace = WorkspaceEntry {
            id: "workspace-1".to_string(),
            project_id: "project-1".to_string(),
            kind: WorkspaceKind::Worktree,
            name: "feature-branch".to_string(),
            display_name: None,
            branch: "feature-branch".to_string(),
            location: WorkspaceLocation::Local {
                worktree_path: PathBuf::from("/tmp/feature-branch"),
            },
            agent_preset_id: "codex".to_string(),
            managed: true,
            git_status: WorkspaceGitStatus::Available,
            git_summary: None,
            attention_status: WorkspaceAttentionStatus::Idle,
            review_pending: false,
            last_attention_reason: None,
            created_at: Default::default(),
            last_opened_at: Default::default(),
        };
        let preset = AgentPreset {
            id: "codex".to_string(),
            label: "Codex".to_string(),
            launch_mode: PresetLaunchMode::Terminal,
            command: "codex".to_string(),
            args: vec!["--help".to_string()],
            env: Default::default(),
            acp_agent_name: Some("codex-acp".to_string()),
            attention_patterns: Vec::new(),
        };

        let launch = prepare_workspace_launch(&workspace, &preset)
            .expect("managed terminal launch should succeed");

        assert_eq!(launch.command, "codex");
        assert_eq!(
            launch
                .environment
                .get(AGENT_REAL_CODEX_BIN_ENV_VAR)
                .map(String::as_str),
            Some("codex")
        );
        assert_eq!(launch.args, vec!["--help".to_string()]);
    }
}
