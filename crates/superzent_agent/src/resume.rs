use std::path::{Path, PathBuf};

use crate::AgentKind;

#[derive(Clone, Copy, PartialEq)]
enum OptionValue {
    None,
    Required,
    Optional,
    // Takes every following argument up to the next option, as the agent's CLI does.
    Variadic,
}

#[derive(Clone, Copy, PartialEq)]
enum OnResume {
    Keep,
    // Picks or creates the session, or only applies to its first prompt.
    Drop,
    // The session it starts isn't an interactive one.
    NotResumable,
}

struct CliOption {
    names: &'static [&'static str],
    value: OptionValue,
    on_resume: OnResume,
}

const fn option(
    names: &'static [&'static str],
    value: OptionValue,
    on_resume: OnResume,
) -> CliOption {
    CliOption {
        names,
        value,
        on_resume,
    }
}

// Only options that take values or must not carry over are listed. Anything else is kept
// as a flag, unless an argument follows it that could be its value.
const CLAUDE_OPTIONS: &[CliOption] = &[
    option(
        &[
            "--allow-dangerously-skip-permissions",
            "--ax-screen-reader",
            "--bare",
            "--brief",
            "--chrome",
            "--dangerously-skip-permissions",
            "--disable-slash-commands",
            "--exclude-dynamic-system-prompt-sections",
            "--forward-subagent-text",
            "--ide",
            "--include-hook-events",
            "--include-partial-messages",
            "--no-chrome",
            "--no-session-persistence",
            "--replay-user-messages",
            "--restricted",
            "--safe-mode",
            "--strict-mcp-config",
            "--verbose",
        ],
        OptionValue::None,
        OnResume::Keep,
    ),
    option(&["--add-dir"], OptionValue::Variadic, OnResume::Keep),
    option(&["--agent"], OptionValue::Required, OnResume::Keep),
    option(&["--agents"], OptionValue::Required, OnResume::Keep),
    option(
        &["--allowedTools", "--allowed-tools"],
        OptionValue::Variadic,
        OnResume::Keep,
    ),
    option(
        &["--append-system-prompt"],
        OptionValue::Required,
        OnResume::Keep,
    ),
    option(
        &["--append-system-prompt-file"],
        OptionValue::Required,
        OnResume::Keep,
    ),
    option(&["--autocompact"], OptionValue::Required, OnResume::Keep),
    option(
        &["--bg", "--background"],
        OptionValue::None,
        OnResume::NotResumable,
    ),
    option(&["--betas"], OptionValue::Variadic, OnResume::Keep),
    option(&["--cloud"], OptionValue::Optional, OnResume::NotResumable),
    option(&["-c", "--continue"], OptionValue::None, OnResume::Drop),
    option(&["-d", "--debug"], OptionValue::Optional, OnResume::Keep),
    option(&["--debug-file"], OptionValue::Required, OnResume::Keep),
    option(&["--desktop"], OptionValue::None, OnResume::NotResumable),
    option(
        &["--disallowedTools", "--disallowed-tools"],
        OptionValue::Variadic,
        OnResume::Keep,
    ),
    option(&["--effort"], OptionValue::Required, OnResume::Keep),
    option(
        &["--environment"],
        OptionValue::Required,
        OnResume::NotResumable,
    ),
    option(&["--fallback-model"], OptionValue::Required, OnResume::Keep),
    option(&["--file"], OptionValue::Variadic, OnResume::Drop),
    option(&["--fork-session"], OptionValue::None, OnResume::Drop),
    option(&["--from-pr"], OptionValue::Optional, OnResume::Drop),
    option(&["--input-format"], OptionValue::Required, OnResume::Keep),
    option(&["--json-schema"], OptionValue::Required, OnResume::Keep),
    option(&["--max-budget-usd"], OptionValue::Required, OnResume::Keep),
    option(&["--mcp-config"], OptionValue::Variadic, OnResume::Keep),
    option(&["--model"], OptionValue::Required, OnResume::Keep),
    option(&["-n", "--name"], OptionValue::Required, OnResume::Drop),
    option(&["--output-format"], OptionValue::Required, OnResume::Keep),
    option(
        &["--permission-mode"],
        OptionValue::Required,
        OnResume::Keep,
    ),
    option(
        &["--permission-prompts"],
        OptionValue::Required,
        OnResume::Keep,
    ),
    option(&["--plugin-dir"], OptionValue::Required, OnResume::Keep),
    option(&["--plugin-url"], OptionValue::Required, OnResume::Keep),
    option(
        &["-p", "--print"],
        OptionValue::None,
        OnResume::NotResumable,
    ),
    option(
        &["--prompt-suggestions"],
        OptionValue::Optional,
        OnResume::Keep,
    ),
    option(&["--remote-control"], OptionValue::Optional, OnResume::Keep),
    option(
        &["--remote-control-session-name-prefix"],
        OptionValue::Required,
        OnResume::Keep,
    ),
    option(&["-r", "--resume"], OptionValue::Optional, OnResume::Drop),
    option(&["--session-id"], OptionValue::Required, OnResume::Drop),
    option(
        &["--setting-sources"],
        OptionValue::Required,
        OnResume::Keep,
    ),
    option(&["--settings"], OptionValue::Required, OnResume::Keep),
    option(&["--system-prompt"], OptionValue::Required, OnResume::Keep),
    option(
        &["--system-prompt-file"],
        OptionValue::Required,
        OnResume::Keep,
    ),
    option(
        &["--system-prompt-snapshot"],
        OptionValue::Required,
        OnResume::Keep,
    ),
    option(&["--teleport"], OptionValue::Optional, OnResume::Drop),
    option(&["--tmux"], OptionValue::Optional, OnResume::Drop),
    option(&["--tools"], OptionValue::Variadic, OnResume::Keep),
    option(&["-w", "--worktree"], OptionValue::Optional, OnResume::Drop),
];

const CODEX_OPTIONS: &[CliOption] = &[
    option(
        &[
            "--approve-for-me",
            "--dangerously-bypass-approvals-and-sandbox",
            "--dangerously-bypass-hook-trust",
            "--no-alt-screen",
            "--no-daemon",
            "--oss",
            "--search",
            "--strict-config",
        ],
        OptionValue::None,
        OnResume::Keep,
    ),
    option(
        &["-a", "--ask-for-approval"],
        OptionValue::Required,
        OnResume::Keep,
    ),
    option(&["--add-dir"], OptionValue::Required, OnResume::Keep),
    option(&["--all"], OptionValue::None, OnResume::Drop),
    option(&["-C", "--cd"], OptionValue::Required, OnResume::Keep),
    option(&["-c", "--config"], OptionValue::Required, OnResume::Keep),
    option(&["--disable"], OptionValue::Required, OnResume::Keep),
    option(&["--enable"], OptionValue::Required, OnResume::Keep),
    option(&["-i", "--image"], OptionValue::Variadic, OnResume::Drop),
    option(
        &["--include-non-interactive"],
        OptionValue::None,
        OnResume::Drop,
    ),
    option(&["--last"], OptionValue::None, OnResume::Drop),
    option(&["--local-provider"], OptionValue::Required, OnResume::Keep),
    option(&["-m", "--model"], OptionValue::Required, OnResume::Keep),
    option(&["-p", "--profile"], OptionValue::Required, OnResume::Keep),
    option(&["--remote"], OptionValue::Required, OnResume::Keep),
    option(
        &["--remote-auth-token-env"],
        OptionValue::Required,
        OnResume::Keep,
    ),
    option(&["-s", "--sandbox"], OptionValue::Required, OnResume::Keep),
    option(&["--worktree"], OptionValue::None, OnResume::Drop),
];

// Subcommands that continue an interactive session; any other subcommand isn't one.
const CODEX_SESSION_SUBCOMMANDS: &[&str] = &["resume", "fork"];
const CODEX_SUBCOMMANDS: &[&str] = &[
    "a",
    "agents",
    "app",
    "app-server",
    "apply",
    "archive",
    "cloud",
    "completion",
    "debug",
    "delete",
    "doctor",
    "e",
    "exec",
    "exec-server",
    "features",
    "help",
    "login",
    "logout",
    "mcp",
    "mcp-server",
    "migrate-rollouts",
    "plugin",
    "queue",
    "remote-control",
    "review",
    "sandbox",
    "unarchive",
    "update",
];

struct LaunchArgs {
    options: Vec<String>,
    // Only those before a `--`; anything after it is a prompt, never a subcommand.
    positionals: Vec<String>,
}

fn split_launch_args(args: &[String], known_options: &[CliOption]) -> Option<LaunchArgs> {
    let mut options = Vec::new();
    let mut positionals = Vec::new();
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        index += 1;
        if arg == "--" {
            break;
        }
        if !arg.starts_with('-') || arg == "-" {
            positionals.push(arg.clone());
            continue;
        }
        let start = index - 1;
        let (name, has_inline_value) = match arg.split_once('=') {
            Some((name, _)) if name.starts_with("--") => (name, true),
            _ => (arg.as_str(), false),
        };
        let known_option = known_options
            .iter()
            .find(|known_option| known_option.names.contains(&name));
        let takes_next = |index: usize| args.get(index).is_some_and(|next| !next.starts_with('-'));
        if !has_inline_value {
            match known_option.map_or(OptionValue::None, |option| option.value) {
                OptionValue::None => {}
                OptionValue::Required => index = (index + 1).min(args.len()),
                OptionValue::Optional => {
                    if takes_next(index) {
                        index += 1;
                    }
                }
                OptionValue::Variadic => {
                    while takes_next(index) {
                        index += 1;
                    }
                }
            }
        }
        let on_resume = match known_option {
            Some(option) => option.on_resume,
            // Without the value it may take, the option would take the resume arguments
            // instead.
            None if !has_inline_value && takes_next(index) => {
                log::debug!("not resuming with {name}, which may take a value");
                OnResume::Drop
            }
            None => OnResume::Keep,
        };
        match on_resume {
            OnResume::Keep => options.extend(args[start..index].iter().cloned()),
            OnResume::Drop => {}
            OnResume::NotResumable => return None,
        }
    }
    Some(LaunchArgs {
        options,
        positionals,
    })
}

impl AgentKind {
    /// The command that continues one of this agent's sessions, started with
    /// `launch_args`, in the directory it ran in. The launch's settings carry over; its
    /// prompt and anything that picked its session don't.
    pub fn resume_command(self, session_id: &str, launch_args: &[String]) -> Option<String> {
        // The id arrives in a hook request and is typed into a shell.
        let is_plain_id = (1..=128).contains(&session_id.len())
            && session_id.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '-' | '_')
            });
        if !is_plain_id {
            return None;
        }
        let (binary, known_options) = match self {
            Self::Claude => ("claude", CLAUDE_OPTIONS),
            Self::Codex => ("codex", CODEX_OPTIONS),
        };
        let launch_args = split_launch_args(launch_args, known_options)?;
        if self == Self::Codex
            && let Some(subcommand) = launch_args.positionals.first()
            && CODEX_SUBCOMMANDS.contains(&subcommand.as_str())
            && !CODEX_SESSION_SUBCOMMANDS.contains(&subcommand.as_str())
        {
            return None;
        }
        let resume_args = match self {
            Self::Claude => ["--resume", session_id],
            Self::Codex => ["resume", session_id],
        };
        let words = std::iter::once(binary.to_string())
            .chain(launch_args.options.iter().map(|arg| shell_word(arg)))
            .chain(resume_args.iter().map(|arg| arg.to_string()))
            .collect::<Vec<_>>();
        Some(words.join(" "))
    }
}

impl AgentKind {
    /// The session a launch continues by id. Codex names its session only once a turn
    /// completes, so a resumed one would otherwise go unrecorded until then.
    pub fn resumed_session_id(self, launch_args: &[String]) -> Option<String> {
        match self {
            // Claude names its session as soon as it starts.
            Self::Claude => None,
            Self::Codex => {
                let launch_args = split_launch_args(launch_args, CODEX_OPTIONS)?;
                match launch_args.positionals.as_slice() {
                    [subcommand, session_id, ..] if subcommand == "resume" => {
                        Some(session_id.clone())
                    }
                    _ => None,
                }
            }
        }
    }
}

/// Whether Codex saved the session, so `codex resume` can find it. Codex also reports the
/// turns of threads it never saves, like the one that titles a conversation.
/// `codex_home` is the agent's own `CODEX_HOME`, which can differ from Superzent's.
pub fn codex_session_is_saved(session_id: &str, codex_home: Option<&Path>) -> bool {
    let codex_home = codex_home
        .map(Path::to_path_buf)
        .or_else(|| std::env::var_os("CODEX_HOME").map(PathBuf::from))
        .unwrap_or_else(|| paths::home_dir().join(".codex"));
    codex_session_is_saved_in(&codex_home, session_id)
}

fn codex_session_is_saved_in(codex_home: &Path, session_id: &str) -> bool {
    let file_suffix = format!("-{session_id}.jsonl");
    // Sessions are filed by the local date they started on, which their time-ordered id
    // gives to within a day.
    let earliest_day = session_id_start_day(session_id);
    let newest_first = |directory: &Path| -> Vec<(String, PathBuf)> {
        let entries = match std::fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) => {
                log::debug!("can't list Codex sessions in {directory:?}: {error}");
                return Vec::new();
            }
        };
        let mut entries = entries
            .filter_map(|entry| {
                let entry = entry.ok()?;
                Some((entry.file_name().to_str()?.to_string(), entry.path()))
            })
            .collect::<Vec<_>>();
        entries.sort_by(|left, right| right.0.cmp(&left.0));
        entries
    };
    for (year, year_path) in newest_first(&codex_home.join("sessions")) {
        for (month, month_path) in newest_first(&year_path) {
            for (day, day_path) in newest_first(&month_path) {
                let date = format!("{year}/{month}/{day}");
                if earliest_day
                    .as_ref()
                    .is_some_and(|earliest_day| date < *earliest_day)
                {
                    return false;
                }
                if newest_first(&day_path)
                    .iter()
                    .any(|(file_name, _)| file_name.ends_with(&file_suffix))
                {
                    return true;
                }
            }
        }
    }
    false
}

/// The day before a UUIDv7 session id was made, as Codex names its session directories.
fn session_id_start_day(session_id: &str) -> Option<String> {
    let timestamp_hex = session_id.replace('-', "");
    let milliseconds = i64::from_str_radix(timestamp_hex.get(..12)?, 16).ok()?;
    let started = chrono::DateTime::from_timestamp_millis(milliseconds)?;
    let day_before = started.date_naive().pred_opt()?;
    Some(day_before.format("%Y/%m/%d").to_string())
}

/// Quotes an argument so bash, zsh, and fish all read it back unchanged.
fn shell_word(arg: &str) -> String {
    let is_plain = !arg.is_empty()
        && arg.chars().all(|character| {
            character.is_ascii_alphanumeric()
                || matches!(
                    character,
                    '-' | '_' | '.' | '/' | '=' | ':' | ',' | '@' | '+' | '%'
                )
        });
    if is_plain {
        arg.to_string()
    } else {
        format!("'{}'", arg.replace('\'', "'\"'\"'"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| arg.to_string()).collect()
    }

    #[test]
    fn resume_commands_only_accept_plain_session_ids() {
        assert_eq!(
            AgentKind::Claude
                .resume_command("0c8f6d1e-2b4a-4f7e-9a51-3d2e8c7b6a90", &[])
                .as_deref(),
            Some("claude --resume 0c8f6d1e-2b4a-4f7e-9a51-3d2e8c7b6a90")
        );
        assert_eq!(
            AgentKind::Codex.resume_command("019a2b3c", &[]).as_deref(),
            Some("codex resume 019a2b3c")
        );
        assert_eq!(AgentKind::Claude.resume_command("", &[]), None);
        assert_eq!(AgentKind::Claude.resume_command("abc; rm -rf ~", &[]), None);
        assert_eq!(AgentKind::Codex.resume_command("$(whoami)", &[]), None);
    }

    #[test]
    fn claude_resumes_with_its_settings_but_not_its_prompt() {
        let launch = args(&[
            "--dangerously-skip-permissions",
            "--model",
            "opus",
            "--allowedTools",
            "Bash(git:*)",
            "Edit",
            "--continue",
            "--append-system-prompt",
            "Don't push",
            "Fix the flaky test",
        ]);
        assert_eq!(
            AgentKind::Claude.resume_command("1b2c", &launch).as_deref(),
            Some(
                "claude --dangerously-skip-permissions --model opus --allowedTools 'Bash(git:*)' Edit --append-system-prompt 'Don'\"'\"'t push' --resume 1b2c"
            )
        );
        assert_eq!(
            AgentKind::Claude
                .resume_command(
                    "1b2c",
                    &args(&[
                        "--resume",
                        "old",
                        "--permission-mode=plan",
                        "-w",
                        "--",
                        "-x"
                    ])
                )
                .as_deref(),
            Some("claude --permission-mode=plan --resume 1b2c")
        );
    }

    #[test]
    fn options_that_may_take_a_value_never_take_the_resume_arguments() {
        assert_eq!(
            AgentKind::Claude
                .resume_command(
                    "1b2c",
                    &args(&[
                        "--system-prompt-file",
                        "./prompt.md",
                        "--unknown-value",
                        "x",
                        "--unknown-flag",
                        "--verbose",
                        "--unknown-inline=y",
                    ])
                )
                .as_deref(),
            Some(
                "claude --system-prompt-file ./prompt.md --unknown-flag --verbose --unknown-inline=y --resume 1b2c"
            )
        );
        // Known flags keep their place ahead of a prompt.
        assert_eq!(
            AgentKind::Claude
                .resume_command("1b2c", &args(&["--dangerously-skip-permissions", "Fix it"]))
                .as_deref(),
            Some("claude --dangerously-skip-permissions --resume 1b2c")
        );
        assert_eq!(
            AgentKind::Codex
                .resume_command("019a", &args(&["--search", "Fix it"]))
                .as_deref(),
            Some("codex --search resume 019a")
        );
    }

    #[test]
    fn codex_resumes_with_its_settings_but_not_its_prompt() {
        assert_eq!(
            AgentKind::Codex
                .resume_command(
                    "019a",
                    &args(&[
                        "--dangerously-bypass-approvals-and-sandbox",
                        "-m",
                        "gpt-5",
                        "-c",
                        "model_reasoning_effort=\"high\"",
                        "-i",
                        "screenshot.png",
                        "Rename the store",
                    ])
                )
                .as_deref(),
            Some(
                "codex --dangerously-bypass-approvals-and-sandbox -m gpt-5 -c 'model_reasoning_effort=\"high\"' resume 019a"
            )
        );
        assert_eq!(
            AgentKind::Codex
                .resume_command("019a", &args(&["resume", "--last", "--search"]))
                .as_deref(),
            Some("codex --search resume 019a")
        );
        // A prompt that reads like a subcommand.
        assert_eq!(
            AgentKind::Codex
                .resume_command("019a", &args(&["--search", "--", "review"]))
                .as_deref(),
            Some("codex --search resume 019a")
        );
    }

    #[test]
    fn only_sessions_codex_saved_are_resumable() {
        let codex_home = tempfile::tempdir().expect("create Codex home");
        let day = codex_home.path().join("sessions/2026/10/05");
        std::fs::create_dir_all(&day).expect("create session directory");
        std::fs::write(
            day.join("rollout-2026-10-05T23-34-48-01a10c7d-1bb3-7341-9c38-8b490c4dcd6d.jsonl"),
            "",
        )
        .expect("write session");
        std::fs::create_dir_all(codex_home.path().join("sessions/2026/10/06"))
            .expect("create later session directory");

        assert!(codex_session_is_saved_in(
            codex_home.path(),
            "01a10c7d-1bb3-7341-9c38-8b490c4dcd6d"
        ));
        // The thread that titled the conversation.
        assert!(!codex_session_is_saved_in(
            codex_home.path(),
            "01a10c7d-25e3-70e0-90a3-4e70560cf1e7"
        ));
        assert_eq!(
            session_id_start_day("01a10c7d-1bb3-7341-9c38-8b490c4dcd6d").as_deref(),
            Some("2026/10/04")
        );
    }

    #[test]
    fn a_codex_launch_names_the_session_it_resumes() {
        assert_eq!(
            AgentKind::Codex
                .resumed_session_id(&args(&["-m", "gpt-5", "resume", "019a", "continue"]))
                .as_deref(),
            Some("019a")
        );
        assert_eq!(
            AgentKind::Codex.resumed_session_id(&args(&["resume", "--last"])),
            None
        );
        assert_eq!(
            AgentKind::Claude.resumed_session_id(&args(&["--resume", "1b2c"])),
            None
        );
    }

    #[test]
    fn sessions_that_are_not_interactive_are_not_resumed() {
        assert_eq!(
            AgentKind::Claude.resume_command("1b2c", &args(&["-p", "summarize"])),
            None
        );
        assert_eq!(
            AgentKind::Codex.resume_command("019a", &args(&["exec", "summarize"])),
            None
        );
    }
}
