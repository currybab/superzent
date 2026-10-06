use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use collections::{BTreeMap, HashMap};
use serde::Deserialize;
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    thread,
    time::Duration,
};
use superzent_model::{AgentPreset, AgentSession, PresetLaunchMode, WorkspaceEntry};
use task::{HideStrategy, RevealStrategy, RevealTarget, Shell, SpawnInTerminal, TaskId};
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
const AGENT_KIND_ENV_VAR: &str = "SUPERZENT_AGENT_KIND";

const HOOK_ENDPOINT_PATH: &str = "/agent-hook";
const PROMPT_TITLE_MAX_CHARS: usize = 120;
const NOTIFY_SCRIPT_FILE_NAME: &str = "notify.sh";
const WRAPPER_MARKER: &str = "# Superzent agent wrapper v1";

const WRAPPER_NOTIFICATION_SCOPE: &str = r#"
# A nested agent belongs to its outer agent's request, not a new terminal task.
if [ -n "$SUPERZENT_TERMINAL_ID" ]; then
  if [ "${SUPERZENT_HOOK_OWNER_TERMINAL_ID:-}" = "$SUPERZENT_TERMINAL_ID" ]; then
    export SUPERZENT_SUPPRESS_AGENT_COMPLETION=1
  else
    export SUPERZENT_HOOK_OWNER_TERMINAL_ID="$SUPERZENT_TERMINAL_ID"
    unset SUPERZENT_SUPPRESS_AGENT_COMPLETION
  fi
fi
"#;

// The arguments the agent was started with, so a restart can resume it the same way. They
// are NUL-separated, which no argument can contain, and encoded to fit in a variable.
const WRAPPER_LAUNCH_ARGS: &str = r#"
if [ "$#" -gt 0 ]; then
  export SUPERZENT_AGENT_ARGS="$(printf '%s\0' "$@" | base64 | tr -d '\n')"
else
  unset SUPERZENT_AGENT_ARGS
fi
"#;

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
    // The agent launched or exited; neither says anything about work in progress.
    SessionStart,
    SessionEnd,
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
    pub agent: Option<AgentKind>,
    /// The first line of the user's prompt, when the hook payload carries one.
    pub prompt: Option<String>,
    /// The arguments the agent was started with, when its wrapper reported them.
    pub launch_args: Option<Vec<String>>,
    /// Where Codex keeps its sessions, when the agent's environment sets it.
    pub codex_home: Option<PathBuf>,
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

/// Whether Superzent wraps this agent command so it reports lifecycle hook events.
pub fn reports_lifecycle_hooks(command: &str) -> bool {
    AgentKind::for_command(command).is_some()
}

pub fn new_terminal_id() -> String {
    Uuid::new_v4().to_string()
}

pub fn inject_terminal_environment(environment: &mut HashMap<String, String>) -> Result<String> {
    let runtime = runtime()?;
    let terminal_id = new_terminal_id();

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

    let managed_command = AgentKind::for_command(&preset.command);
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

/// The part of a preset terminal's environment the preset chose, which a restored terminal
/// needs to run its agent the same way. Superzent's own variables are set anew for every
/// terminal, and the shell builds its own `PATH`.
pub fn agent_launch_environment(environment: &HashMap<String, String>) -> BTreeMap<String, String> {
    environment
        .iter()
        .filter(|(key, _)| {
            let is_superzent_variable = key.starts_with("SUPERZENT_")
                && key.as_str() != AGENT_REAL_CLAUDE_BIN_ENV_VAR
                && key.as_str() != AGENT_REAL_CODEX_BIN_ENV_VAR;
            !is_superzent_variable && key.as_str() != "PATH"
        })
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
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

        let listener = TcpListener::bind("127.0.0.1:0").context("bind hook port")?;
        let hook_url = format!(
            "http://127.0.0.1:{}{}",
            listener.local_addr().context("read hook port")?.port(),
            HOOK_ENDPOINT_PATH
        );

        let subscribers = Arc::new(Mutex::new(Vec::new()));
        spawn_hook_server(listener, subscribers.clone());

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

// This server intentionally avoids an HTTP library: hook notifications arrive from
// short-lived `curl --max-time` invocations that routinely disconnect mid-request when
// the app is busy, and tiny_http panicked (aborting the whole app) on such connections.
fn spawn_hook_server(
    listener: TcpListener,
    subscribers: Arc<Mutex<Vec<smol::channel::Sender<AgentHookEvent>>>>,
) {
    thread::Builder::new()
        .name("superzent-agent-hooks".to_string())
        .spawn(move || {
            loop {
                let stream = match listener.accept() {
                    Ok((stream, _)) => stream,
                    Err(error) => {
                        log::debug!("agent hook server failed to accept connection: {error}");
                        thread::sleep(Duration::from_millis(100));
                        continue;
                    }
                };
                let subscribers = subscribers.clone();
                thread::Builder::new()
                    .name("superzent-agent-hook-conn".to_string())
                    .spawn(move || {
                        if let Err(error) = handle_hook_connection(&stream, &subscribers) {
                            log::debug!("failed to handle agent hook connection: {error:#}");
                        }
                    })
                    .ok();
            }
        })
        .ok();
}

fn handle_hook_connection(
    stream: &TcpStream,
    subscribers: &Mutex<Vec<smol::channel::Sender<AgentHookEvent>>>,
) -> Result<()> {
    const MAX_REQUEST_HEAD_BYTES: u64 = 16 * 1024;
    // Payloads can carry a whole prompt or Codex's last reply; anything past this is
    // dropped, which only costs the prompt title.
    const MAX_REQUEST_BODY_BYTES: u64 = 256 * 1024;

    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .context("set hook connection read timeout")?;
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .context("set hook connection write timeout")?;

    let mut reader = BufReader::new(stream);
    let mut head = (&mut reader).take(MAX_REQUEST_HEAD_BYTES);
    let mut request_line = String::new();
    head.read_line(&mut request_line)
        .context("read hook request line")?;
    let url = request_line
        .split_whitespace()
        .nth(1)
        .context("malformed hook request line")?
        .to_string();

    // Drain the remaining headers so curl finishes sending before we respond and close.
    let mut content_length = 0;
    let mut line = String::new();
    loop {
        line.clear();
        match head.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) if line == "\r\n" || line == "\n" => break,
            Ok(_) => {
                if let Some((name, value)) = line.split_once(':')
                    && name.trim().eq_ignore_ascii_case("content-length")
                {
                    content_length = value.trim().parse::<u64>().unwrap_or(0);
                }
            }
            Err(error) => {
                log::debug!("failed to read agent hook request headers: {error}");
                break;
            }
        }
    }

    let mut body = Vec::new();
    if content_length > 0 {
        reader
            .take(content_length.min(MAX_REQUEST_BODY_BYTES))
            .read_to_end(&mut body)
            .context("read hook request body")?;
    }
    let body = String::from_utf8_lossy(&body);

    let status_line = match parse_request(&url, Some(body.as_ref()).filter(|body| !body.is_empty()))
    {
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
                subscribers.retain(|sender| sender.send_blocking(event.clone()).is_ok());
            }
            "204 No Content"
        }
        Ok(None) => {
            if debug_hooks_enabled() {
                log::info!("superzent hook server ignored request: url={url}");
            }
            "204 No Content"
        }
        Err(error) => {
            log::warn!("failed to parse agent hook request: {error:#}");
            "400 Bad Request"
        }
    };

    let mut stream = stream;
    stream
        .write_all(format!("HTTP/1.1 {status_line}\r\nConnection: close\r\n\r\n").as_bytes())
        .context("respond to hook request")?;
    stream.flush().context("flush hook response")?;
    Ok(())
}

/// Hook parameters come in the form body from the notify script, or in the query string
/// from older wrappers that are still running.
fn parse_request(url: &str, body: Option<&str>) -> Result<Option<AgentHookEvent>> {
    let url =
        Url::parse(&format!("http://127.0.0.1{url}")).context("failed to parse agent hook url")?;
    if url.path() != HOOK_ENDPOINT_PATH {
        if debug_hooks_enabled() {
            log::info!("superzent hook parse skipped non-hook path: {}", url.path());
        }
        return Ok(None);
    }

    let query = body.unwrap_or_else(|| url.query().unwrap_or_default());
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
        agent: params.agent.as_deref().and_then(AgentKind::from_hook_value),
        prompt: params.payload.as_deref().and_then(prompt_from_hook_payload),
        launch_args: params
            .agent_args
            .as_deref()
            .filter(|agent_args| !agent_args.is_empty())
            .and_then(decode_launch_args),
        codex_home: params
            .codex_home
            .filter(|codex_home| !codex_home.is_empty())
            .map(PathBuf::from),
    }))
}

#[derive(Debug, Deserialize)]
struct HookRequestParams {
    #[serde(rename = "agent")]
    agent: Option<String>,
    #[serde(rename = "agent_args")]
    agent_args: Option<String>,
    #[serde(rename = "codex_home")]
    codex_home: Option<String>,
    #[serde(rename = "cwd")]
    cwd: Option<String>,
    #[serde(rename = "event_type")]
    event_type: Option<String>,
    #[serde(rename = "payload")]
    payload: Option<String>,
    #[serde(rename = "session_id")]
    session_id: Option<String>,
    #[serde(rename = "terminal_id")]
    terminal_id: Option<String>,
    #[serde(rename = "version")]
    version: Option<String>,
    #[serde(rename = "workspace_id")]
    workspace_id: Option<String>,
}

fn decode_launch_args(encoded: &str) -> Option<Vec<String>> {
    let decoded = match base64::engine::general_purpose::STANDARD.decode(encoded) {
        Ok(decoded) => decoded,
        Err(error) => {
            log::warn!("ignoring undecodable agent launch arguments: {error}");
            return None;
        }
    };
    let Some(arguments) = decoded.strip_suffix(&[0]) else {
        log::warn!("ignoring agent launch arguments without a terminator");
        return None;
    };
    arguments
        .split(|byte| *byte == 0)
        .map(|argument| String::from_utf8(argument.to_vec()).ok())
        .collect()
}

/// Claude sends the prompt as `prompt` when it is submitted; Codex sends the turn's
/// messages as `input-messages` when the turn completes.
fn prompt_from_hook_payload(payload: &str) -> Option<String> {
    let payload: serde_json::Value = serde_json::from_str(payload).ok()?;
    let prompt = payload
        .get("prompt")
        .and_then(serde_json::Value::as_str)
        .or_else(|| payload.get("input-messages")?.as_array()?.first()?.as_str())?;
    let first_line = prompt
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())?;
    Some(first_line.chars().take(PROMPT_TITLE_MAX_CHARS).collect())
}

fn map_hook_event_type(event_type: &str) -> Option<AgentHookEventType> {
    match event_type {
        "Start"
        | "UserPromptSubmit"
        | "PostToolUse"
        | "PostToolUseFailure"
        | "BeforeAgent"
        | "AfterTool"
        | "userPromptSubmitted"
        | "postToolUse" => Some(AgentHookEventType::Start),
        "PermissionRequest" | "preToolUse" | "Notification" => {
            Some(AgentHookEventType::PermissionRequest)
        }
        "Stop" | "AfterAgent" | "agent-turn-complete" | "sessionEnd" => {
            Some(AgentHookEventType::Stop)
        }
        "SessionStart" => Some(AgentHookEventType::SessionStart),
        "SessionEnd" => Some(AgentHookEventType::SessionEnd),
        _ => None,
    }
}

/// An agent CLI that Superzent wraps so it reports lifecycle hooks.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentKind {
    Claude,
    Codex,
}

impl AgentKind {
    fn from_hook_value(value: &str) -> Option<Self> {
        match value {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            _ => None,
        }
    }

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

if [ "${SUPERZENT_SUPPRESS_AGENT_COMPLETION:-}" = "1" ]; then
  case "$EVENT_TYPE" in
    Stop|AfterAgent|agent-turn-complete|sessionEnd|SessionStart|SessionEnd) exit 0 ;;
  esac
fi

# Claude names its session in every payload; Codex names its thread when a turn completes.
_superzent_session_id=$(printf '%s\n' "$INPUT" | grep -oE '"(session_id|thread-id)"[[:space:]]*:[[:space:]]*"[^"]*"' | head -n 1 | grep -oE '"[^"]*"$' | tr -d '"')

# Only these payloads carry the prompt; tool events carry tool output, which can be
# large, and Claude waits for every hook to finish.
_superzent_payload=""
case "$EVENT_TYPE" in
  UserPromptSubmit|agent-turn-complete) _superzent_payload="$INPUT" ;;
esac

# The payload goes through stdin: as an argument, a long final reply can exceed the
# command-line length limit and keep curl from starting at all.
_superzent_status=$(printf '%s' "$_superzent_payload" | curl -sS "$SUPERZENT_AGENT_HOOK_URL" \
  --connect-timeout 1 \
  --max-time 2 \
  -H 'Expect:' \
  --data-urlencode "event_type=$EVENT_TYPE" \
  --data-urlencode "terminal_id=$SUPERZENT_TERMINAL_ID" \
  --data-urlencode "workspace_id=$SUPERZENT_WORKSPACE_ID" \
  --data-urlencode "session_id=$_superzent_session_id" \
  --data-urlencode "cwd=$PWD" \
  --data-urlencode "agent=${SUPERZENT_AGENT_KIND:-}" \
  --data-urlencode "agent_args=${SUPERZENT_AGENT_ARGS:-}" \
  --data-urlencode "codex_home=${CODEX_HOME:-}" \
  --data-urlencode "version=$SUPERZENT_HOOK_VERSION" \
  --data-urlencode "payload@-" \
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
            "SessionStart": [{ "hooks": [{ "type": "command", "command": notify_command }] }],
            "SessionEnd": [{ "hooks": [{ "type": "command", "command": notify_command }] }],
            "UserPromptSubmit": [{ "hooks": [{ "type": "command", "command": notify_command }] }],
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
      # Skip other Superzent wrapper scripts to prevent cross-instance recursion
      if head -2 "$dir/$name" 2>/dev/null | grep -qF "{WRAPPER_MARKER}"; then
        continue
      fi
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

{WRAPPER_NOTIFICATION_SCOPE}
export {AGENT_KIND_ENV_VAR}=claude
{WRAPPER_LAUNCH_ARGS}
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

{WRAPPER_NOTIFICATION_SCOPE}
export {AGENT_KIND_ENV_VAR}=codex
{WRAPPER_LAUNCH_ARGS}
_superzent_report_session() {{
  if [ -n "$SUPERZENT_TERMINAL_ID" ] && [ -f "{notify_script_path}" ]; then
    bash "{notify_script_path}" "$(printf '{{"hook_event_name":"%s"}}' "$1")" >/dev/null 2>&1 || true
  fi
}}

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

_superzent_report_session SessionStart
"$REAL_BIN" -c "notify=[\"bash\",\"{notify_script_path}\"]" "$@"
SUPERZENT_CODEX_STATUS=$?
_superzent_report_session SessionEnd

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
    fn only_wrapped_agents_report_lifecycle_hooks() {
        assert!(reports_lifecycle_hooks("claude"));
        assert!(reports_lifecycle_hooks("/opt/homebrew/bin/codex"));
        assert!(!reports_lifecycle_hooks("gemini"));
        assert!(!reports_lifecycle_hooks("aider"));
    }

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
            map_hook_event_type("SessionStart"),
            Some(AgentHookEventType::SessionStart)
        );
        assert_eq!(
            map_hook_event_type("SessionEnd"),
            Some(AgentHookEventType::SessionEnd)
        );
        assert_eq!(map_hook_event_type("sessionStart"), None);
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
            "/agent-hook?event_type=Stop&terminal_id=terminal-1&workspace_id=workspace-1&cwd=%2Ftmp%2Fproject&agent=codex&version=1",
            None,
        )
        .expect("request should parse")
        .expect("request should produce an event");

        assert_eq!(event.event_type, AgentHookEventType::Stop);
        assert_eq!(event.terminal_id, "terminal-1");
        assert_eq!(event.workspace_id.as_deref(), Some("workspace-1"));
        assert_eq!(event.cwd.as_deref(), Some(Path::new("/tmp/project")));
        assert_eq!(event.agent, Some(AgentKind::Codex));
    }

    #[test]
    fn hook_requests_without_a_known_agent_have_no_agent_kind() {
        for agent in ["", "&agent=", "&agent=aider"] {
            let event = parse_request(
                &format!("/agent-hook?event_type=Stop&terminal_id=terminal-1{agent}&version=1"),
                None,
            )
            .expect("request should parse")
            .expect("request should produce an event");
            assert_eq!(event.agent, None, "{agent}");
        }
    }

    #[test]
    fn ignores_version_mismatches() {
        let event = parse_request(
            "/agent-hook?event_type=Stop&terminal_id=terminal-1&version=999",
            None,
        )
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
            settings["hooks"]["UserPromptSubmit"][0]["hooks"][0]["type"],
            "command"
        );
        for session_hook in ["SessionStart", "SessionEnd"] {
            assert_eq!(
                settings["hooks"][session_hook][0]["hooks"][0]["type"], "command",
                "Claude should report {session_hook} so idle agents are listed"
            );
        }

        let wrapper =
            codex_wrapper_content(Path::new("/tmp/bin"), Path::new("/tmp/hooks/notify.sh"));
        assert!(wrapper.contains(AGENT_REAL_CODEX_BIN_ENV_VAR));
        assert!(wrapper.contains("notify=[\\\"bash\\\",\\\"/tmp/hooks/notify.sh\\\"]"));
    }

    #[cfg(unix)]
    fn write_test_wrappers(directory: &Path) -> (PathBuf, PathBuf) {
        let bin_dir = directory.join("managed bin");
        fs::create_dir_all(&bin_dir).expect("create test wrapper directory");
        let notify_path = directory.join("hooks/notify.sh");
        let claude = bin_dir.join("claude");
        let codex = bin_dir.join("codex");
        write_executable_file(
            &claude,
            claude_wrapper_content(&bin_dir, &notify_path).expect("render Claude wrapper"),
        )
        .expect("write Claude wrapper");
        write_executable_file(&codex, codex_wrapper_content(&bin_dir, &notify_path))
            .expect("write Codex wrapper");
        (claude, codex)
    }

    #[cfg(unix)]
    #[test]
    fn nested_agents_only_suppress_completion_notifications() {
        let directory = tempfile::tempdir().expect("create wrapper test directory");
        let (claude, codex) = write_test_wrappers(directory.path());
        let parent_binary = directory.path().join("parent-agent");
        let child_binary = directory.path().join("child-agent");
        // The wrappers' own notify path, so the Codex wrapper can report its session.
        let notify_script = directory.path().join("hooks/notify.sh");
        let events_path = directory.path().join("events");
        fs::create_dir_all(directory.path().join("hooks")).expect("create hooks directory");
        write_executable_file(&notify_script, notify_script_content())
            .expect("write notification hook");
        write_executable_file(
            &directory.path().join("curl"),
            r#"#!/bin/bash
for argument in "$@"; do
  case "$argument" in
    event_type=*) _event="${argument#event_type=}" ;;
    agent=*) _agent="${argument#agent=}" ;;
  esac
done
printf '%s:%s\n' "$_event" "$_agent" >> "$SUPERZENT_TEST_EVENTS"
printf '204'
"#
            .into(),
        )
        .expect("write HTTP recorder");
        write_executable_file(
            &parent_binary,
            r#"#!/bin/bash
export SUPERZENT_REAL_CLAUDE_BIN="$SUPERZENT_TEST_CHILD_BINARY"
export SUPERZENT_REAL_CODEX_BIN="$SUPERZENT_TEST_CHILD_BINARY"
"$SUPERZENT_TEST_CHILD_WRAPPER" child-request || exit $?
bash "$SUPERZENT_TEST_NOTIFY_SCRIPT" '{"hook_event_name":"Stop"}'
"#
            .into(),
        )
        .expect("write parent agent");
        write_executable_file(
            &child_binary,
            r#"#!/bin/bash
bash "$SUPERZENT_TEST_NOTIFY_SCRIPT" '{"hook_event_name":"Start"}'
bash "$SUPERZENT_TEST_NOTIFY_SCRIPT" '{"hook_event_name":"PermissionRequest"}'
bash "$SUPERZENT_TEST_NOTIFY_SCRIPT" "$SUPERZENT_TEST_CHILD_STOP"
"#
            .into(),
        )
        .expect("write child agent");

        for (parent, child) in [
            (&claude, &codex),
            (&codex, &claude),
            (&claude, &claude),
            (&codex, &codex),
        ] {
            fs::write(&events_path, "").expect("clear recorded hook events");
            let completion = if child == &codex {
                r#"{"type":"agent-turn-complete"}"#
            } else {
                r#"{"hook_event_name":"Stop"}"#
            };
            let output = smol::block_on(
                smol::process::Command::new(parent)
                    .arg("parent-request")
                    .env(AGENT_REAL_CLAUDE_BIN_ENV_VAR, &parent_binary)
                    .env(AGENT_REAL_CODEX_BIN_ENV_VAR, &parent_binary)
                    .env("SUPERZENT_TEST_CHILD_WRAPPER", child)
                    .env("SUPERZENT_TEST_CHILD_BINARY", &child_binary)
                    .env("SUPERZENT_TEST_CHILD_STOP", completion)
                    .env("SUPERZENT_TEST_NOTIFY_SCRIPT", &notify_script)
                    .env("SUPERZENT_TEST_EVENTS", &events_path)
                    .env(
                        "PATH",
                        format!(
                            "{}:{}",
                            directory.path().display(),
                            std::env::var("PATH").expect("test PATH")
                        ),
                    )
                    .env(AGENT_TERMINAL_ID_ENV_VAR, "terminal-1")
                    .env(AGENT_HOOK_URL_ENV_VAR, "http://127.0.0.1/agent-hook")
                    .env(AGENT_DEBUG_HOOKS_ENV_VAR, "0")
                    .env_remove("SUPERZENT_HOOK_OWNER_TERMINAL_ID")
                    .env_remove("SUPERZENT_SUPPRESS_AGENT_COMPLETION")
                    .output(),
            )
            .expect("run nested agent wrappers");
            assert!(output.status.success(), "{:?}", output);
            let parent_agent = if parent == &codex { "codex" } else { "claude" };
            let child_agent = if child == &codex { "codex" } else { "claude" };
            let child_events = format!("Start:{child_agent}\nPermissionRequest:{child_agent}\n");
            // Codex has no session hooks, so its wrapper reports the session itself.
            let expected = if parent == &codex {
                format!("SessionStart:codex\n{child_events}Stop:codex\nSessionEnd:codex\n")
            } else {
                format!("{child_events}Stop:{parent_agent}\n")
            };
            assert_eq!(
                fs::read_to_string(&events_path).expect("read recorded events"),
                expected,
                "preserve child activity and approvals, but only report completion and the \
                 session for the parent"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn top_level_agent_commands_keep_terminal_notification_hooks() {
        let directory = tempfile::tempdir().expect("create wrapper test directory");
        let (claude, codex) = write_test_wrappers(directory.path());
        let real_binary = directory.path().join("real-agent");
        write_executable_file(&real_binary, "#!/bin/bash\nprintf '%s\\n' \"$@\"\n".into())
            .expect("write real agent");

        for (wrapper, expected_option) in [(&claude, "--settings"), (&codex, "-c")] {
            for inherited_owner in ["", "another-terminal"] {
                let output = smol::block_on(
                    smol::process::Command::new(wrapper)
                        .arg("parent-request")
                        .env(AGENT_REAL_CLAUDE_BIN_ENV_VAR, &real_binary)
                        .env(AGENT_REAL_CODEX_BIN_ENV_VAR, &real_binary)
                        .env(AGENT_TERMINAL_ID_ENV_VAR, "terminal-1")
                        .env(AGENT_DEBUG_HOOKS_ENV_VAR, "0")
                        .env("SUPERZENT_HOOK_OWNER_TERMINAL_ID", inherited_owner)
                        .output(),
                )
                .expect("run top-level agent wrapper");
                assert!(output.status.success(), "{:?}", output);
                let arguments =
                    String::from_utf8(output.stdout).expect("agent arguments are UTF-8");
                assert_eq!(arguments.lines().next(), Some(expected_option));
                assert_eq!(arguments.lines().last(), Some("parent-request"));
            }
        }
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
            teardown_script_override: None,
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
            teardown_script_override: None,
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

    fn spawn_test_hook_server() -> (
        std::net::SocketAddr,
        smol::channel::Receiver<AgentHookEvent>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test listener");
        let addr = listener.local_addr().expect("read test listener addr");
        let (sender, receiver) = smol::channel::unbounded();
        let subscribers = Arc::new(Mutex::new(vec![sender]));
        spawn_hook_server(listener, subscribers);
        (addr, receiver)
    }

    fn read_response(stream: &mut TcpStream) -> String {
        let mut response = String::new();
        stream
            .read_to_string(&mut response)
            .expect("read hook response");
        response
    }

    #[test]
    fn hook_server_dispatches_events_and_survives_aborted_connections() {
        let (addr, receiver) = spawn_test_hook_server();

        // A client that connects and disconnects mid-request (like `curl --max-time`
        // under load) must not take the server down.
        let mut aborted = TcpStream::connect(addr).expect("connect aborted client");
        aborted
            .write_all(b"GET /agent-hook?event_type=Stop")
            .expect("write partial request");
        drop(aborted);

        // Garbage that isn't HTTP at all must not take the server down either.
        let mut garbage = TcpStream::connect(addr).expect("connect garbage client");
        garbage.write_all(b"\r\n\r\n").expect("write garbage");
        drop(garbage);

        let mut stream = TcpStream::connect(addr).expect("connect valid client");
        stream
            .write_all(
                format!(
                    "GET /agent-hook?event_type=Stop&terminal_id=terminal-1&version={AGENT_HOOK_VERSION} HTTP/1.1\r\n\
                     Host: 127.0.0.1\r\n\
                     \r\n"
                )
                .as_bytes(),
            )
            .expect("write valid request");

        let response = read_response(&mut stream);
        assert!(
            response.starts_with("HTTP/1.1 204 No Content"),
            "unexpected response: {response}"
        );

        let event = receiver.recv_blocking().expect("receive hook event");
        assert_eq!(event.event_type, AgentHookEventType::Stop);
        assert_eq!(event.terminal_id, "terminal-1");
    }

    #[test]
    fn hook_requests_carry_the_prompt_from_the_hook_payload() {
        let claude_payload = r#"{"hook_event_name":"UserPromptSubmit","prompt":"\n  Fix the \"flaky\" test\nthen push","session_id":"abc"}"#;
        let codex_payload = r#"{"type":"agent-turn-complete","input-messages":["Rename the store","and more"],"last-assistant-message":"Done"}"#;
        for (payload, expected) in [
            (claude_payload, Some("Fix the \"flaky\" test")),
            (codex_payload, Some("Rename the store")),
            (r#"{"hook_event_name":"Stop"}"#, None),
            ("not json", None),
        ] {
            let body = serde_urlencoded::to_string([
                ("event_type", "Stop"),
                ("terminal_id", "terminal-1"),
                ("payload", payload),
            ])
            .expect("encode hook form");
            let event = parse_request("/agent-hook", Some(&body))
                .expect("request should parse")
                .expect("request should produce an event");
            assert_eq!(event.terminal_id, "terminal-1");
            assert_eq!(event.prompt.as_deref(), expected, "{payload}");
        }
    }

    #[test]
    fn prompts_are_capped_to_a_title_length() {
        let prompt = "가".repeat(PROMPT_TITLE_MAX_CHARS + 50);
        let payload = serde_json::json!({ "prompt": prompt }).to_string();
        let title = prompt_from_hook_payload(&payload).expect("prompt title");
        assert_eq!(title.chars().count(), PROMPT_TITLE_MAX_CHARS);
        assert_eq!(prompt_from_hook_payload(r#"{"prompt":"  \n "}"#), None);
    }

    #[cfg(unix)]
    #[test]
    fn notify_script_delivers_the_hook_payload_to_the_server() {
        use smol::io::AsyncWriteExt as _;

        let (addr, receiver) = spawn_test_hook_server();
        let directory = tempfile::tempdir().expect("create notify test directory");
        let notify_script = directory.path().join("notify.sh");
        write_executable_file(&notify_script, notify_script_content())
            .expect("write notification hook");
        // Larger than a single command-line argument may be on Linux, like a long
        // final reply, so the payload must not reach curl through argv.
        let payload = serde_json::json!({
            "hook_event_name": "UserPromptSubmit",
            "prompt": "Ship it & \"tag\" v0.6.0\nsecond line",
            "last_assistant_message": "a".repeat(150 * 1024),
        })
        .to_string();

        let output = smol::block_on(async {
            let mut child = smol::process::Command::new("bash")
                .arg(&notify_script)
                .env(
                    AGENT_HOOK_URL_ENV_VAR,
                    format!("http://{addr}{HOOK_ENDPOINT_PATH}"),
                )
                .env(AGENT_TERMINAL_ID_ENV_VAR, "terminal-1")
                .env(AGENT_HOOK_VERSION_ENV_VAR, AGENT_HOOK_VERSION)
                .env(AGENT_KIND_ENV_VAR, "claude")
                .env(AGENT_DEBUG_HOOKS_ENV_VAR, "0")
                .stdin(smol::process::Stdio::piped())
                .spawn()
                .expect("run notify script");
            let mut stdin = child.stdin.take().expect("notify script stdin");
            stdin
                .write_all(payload.as_bytes())
                .await
                .expect("write hook payload");
            drop(stdin);
            child.output().await.expect("wait for notify script")
        });
        assert!(output.status.success(), "{output:?}");

        let event = receiver.recv_blocking().expect("receive hook event");
        assert_eq!(event.event_type, AgentHookEventType::Start);
        assert_eq!(event.agent, Some(AgentKind::Claude));
        assert_eq!(event.prompt.as_deref(), Some("Ship it & \"tag\" v0.6.0"));
    }

    #[cfg(unix)]
    #[test]
    fn notify_script_only_sends_payloads_that_carry_the_prompt() {
        let (addr, receiver) = spawn_test_hook_server();
        let directory = tempfile::tempdir().expect("create notify test directory");
        let notify_script = directory.path().join("notify.sh");
        write_executable_file(&notify_script, notify_script_content())
            .expect("write notification hook");

        for (payload, expected_prompt) in [
            // Tool events carry tool output, which can be large and is never needed.
            (
                r#"{"hook_event_name":"PostToolUse","prompt":"not a prompt"}"#,
                None,
            ),
            (
                r#"{"type":"agent-turn-complete","input-messages":["Rename the store"]}"#,
                Some("Rename the store"),
            ),
        ] {
            let output = smol::block_on(
                smol::process::Command::new("bash")
                    .arg(&notify_script)
                    .arg(payload)
                    .env(
                        AGENT_HOOK_URL_ENV_VAR,
                        format!("http://{addr}{HOOK_ENDPOINT_PATH}"),
                    )
                    .env(AGENT_TERMINAL_ID_ENV_VAR, "terminal-1")
                    .env(AGENT_HOOK_VERSION_ENV_VAR, AGENT_HOOK_VERSION)
                    .env(AGENT_DEBUG_HOOKS_ENV_VAR, "0")
                    .output(),
            )
            .expect("run notify script");
            assert!(output.status.success(), "{output:?}");
            let event = receiver.recv_blocking().expect("receive hook event");
            assert_eq!(event.prompt.as_deref(), expected_prompt, "{payload}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn notify_script_reports_the_agent_session() {
        let (addr, receiver) = spawn_test_hook_server();
        let directory = tempfile::tempdir().expect("create notify test directory");
        let notify_script = directory.path().join("notify.sh");
        write_executable_file(&notify_script, notify_script_content())
            .expect("write notification hook");

        for (payload, expected_session_id) in [
            (
                r#"{"hook_event_name":"Stop","session_id":"1b2c-claude","last_assistant_message":"see \"session_id\": \"other\""}"#,
                Some("1b2c-claude"),
            ),
            (
                r#"{"type":"agent-turn-complete","thread-id":"019a-codex","input-messages":["Rename"]}"#,
                Some("019a-codex"),
            ),
            (r#"{"hook_event_name":"SessionStart"}"#, None),
        ] {
            let output = smol::block_on(
                smol::process::Command::new("bash")
                    .arg(&notify_script)
                    .arg(payload)
                    .env(
                        AGENT_HOOK_URL_ENV_VAR,
                        format!("http://{addr}{HOOK_ENDPOINT_PATH}"),
                    )
                    .env(AGENT_TERMINAL_ID_ENV_VAR, "terminal-1")
                    .env(AGENT_HOOK_VERSION_ENV_VAR, AGENT_HOOK_VERSION)
                    .env(AGENT_DEBUG_HOOKS_ENV_VAR, "0")
                    .env("CODEX_HOME", "/work/.codex")
                    .output(),
            )
            .expect("run notify script");
            assert!(output.status.success(), "{output:?}");
            let event = receiver.recv_blocking().expect("receive hook event");
            assert_eq!(
                event.session_id.as_deref(),
                expected_session_id,
                "{payload}"
            );
            assert_eq!(event.codex_home, Some(PathBuf::from("/work/.codex")));
        }
    }

    #[cfg(unix)]
    #[test]
    fn wrappers_report_the_arguments_the_agent_started_with() {
        let launch_args = ["--model", "opus", "Don't \"push\"\nyet", "", "--verbose"];
        let output = smol::block_on(
            smol::process::Command::new("bash")
                .arg("-c")
                .arg(format!(
                    "{WRAPPER_LAUNCH_ARGS}\nprintf '%s' \"$SUPERZENT_AGENT_ARGS\""
                ))
                .arg("wrapper")
                .args(launch_args)
                .output(),
        )
        .expect("run wrapper snippet");
        assert!(output.status.success(), "{output:?}");
        let encoded = String::from_utf8(output.stdout).expect("encoded arguments");
        assert_eq!(
            decode_launch_args(&encoded),
            Some(launch_args.iter().map(|arg| arg.to_string()).collect())
        );
        assert_eq!(decode_launch_args("not base64!"), None);
    }

    #[test]
    fn restored_terminals_keep_only_the_environment_presets_chose() {
        let environment = HashMap::from_iter([
            ("OPENAI_BASE_URL".to_string(), "https://proxy".to_string()),
            ("CODEX_HOME".to_string(), "/work/.codex".to_string()),
            (
                AGENT_REAL_CODEX_BIN_ENV_VAR.to_string(),
                "/opt/codex".to_string(),
            ),
            (
                AGENT_TERMINAL_ID_ENV_VAR.to_string(),
                "terminal-1".to_string(),
            ),
            (
                AGENT_HOOK_URL_ENV_VAR.to_string(),
                "http://hook".to_string(),
            ),
            ("PATH".to_string(), "/hooks:/usr/bin".to_string()),
        ]);
        assert_eq!(
            agent_launch_environment(&environment),
            BTreeMap::from_iter([
                ("CODEX_HOME".to_string(), "/work/.codex".to_string()),
                ("OPENAI_BASE_URL".to_string(), "https://proxy".to_string()),
                (
                    AGENT_REAL_CODEX_BIN_ENV_VAR.to_string(),
                    "/opt/codex".to_string(),
                ),
            ])
        );
    }

    #[test]
    fn hook_server_rejects_unparsable_requests() {
        let (addr, receiver) = spawn_test_hook_server();

        let mut stream = TcpStream::connect(addr).expect("connect client");
        stream
            .write_all(
                format!(
                    "GET /agent-hook?event_type=Stop&version={AGENT_HOOK_VERSION} HTTP/1.1\r\n\r\n"
                )
                .as_bytes(),
            )
            .expect("write request");

        let response = read_response(&mut stream);
        assert!(
            response.starts_with("HTTP/1.1 400 Bad Request"),
            "unexpected response: {response}"
        );
        assert!(receiver.is_empty());
    }
}
