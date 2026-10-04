//! Tells what an agent CLI without lifecycle hooks is doing from what it draws in the
//! terminal. The rules in `agent_detection/` come from herdr (Apache-2.0), and the
//! matcher follows herdr's `src/detect/manifest.rs`.

use std::{path::Path, sync::LazyLock};

use regex::Regex;
use serde::Deserialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScreenAgentState {
    Idle,
    Working,
    Blocked,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScreenDetection {
    pub state: ScreenAgentState,
    /// The screen shows the state outright (a prompt, a spinner) rather than the state
    /// being inferred because no rule matched.
    pub visible: bool,
}

/// An agent CLI recognized by its process, whose state is read from its screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ScreenAgent(usize);

struct AgentSpec {
    id: &'static str,
    display_name: &'static str,
    manifest: &'static str,
    executables: &'static [&'static str],
    // npm packages whose entry script a `node`/`bun` process may run directly.
    packages: &'static [&'static str],
}

const AGENTS: &[AgentSpec] = &[
    AgentSpec {
        id: "amp",
        display_name: "Amp",
        manifest: include_str!("../agent_detection/amp.toml"),
        executables: &["amp", "amp-local"],
        packages: &["@sourcegraph/amp"],
    },
    AgentSpec {
        id: "copilot",
        display_name: "Copilot CLI",
        manifest: include_str!("../agent_detection/github-copilot.toml"),
        executables: &["copilot", "github-copilot", "ghcs"],
        packages: &["@github/copilot"],
    },
    AgentSpec {
        id: "cursor",
        display_name: "Cursor Agent",
        manifest: include_str!("../agent_detection/cursor.toml"),
        executables: &["cursor-agent"],
        packages: &[],
    },
    AgentSpec {
        id: "gemini",
        display_name: "Gemini CLI",
        manifest: include_str!("../agent_detection/gemini.toml"),
        executables: &["gemini"],
        packages: &["@google/gemini-cli"],
    },
    AgentSpec {
        id: "kimi",
        display_name: "Kimi Code",
        manifest: include_str!("../agent_detection/kimi.toml"),
        executables: &["kimi", "kimi-code"],
        packages: &["@moonshot-ai/kimi-code"],
    },
    AgentSpec {
        id: "opencode",
        display_name: "OpenCode",
        manifest: include_str!("../agent_detection/opencode.toml"),
        executables: &["opencode", "opencode2", "open-code"],
        packages: &["opencode-ai"],
    },
    AgentSpec {
        id: "pi",
        display_name: "Pi",
        manifest: include_str!("../agent_detection/pi.toml"),
        executables: &["pi"],
        packages: &["@earendil-works/pi-coding-agent"],
    },
    AgentSpec {
        id: "qwen",
        display_name: "Qwen Code",
        manifest: include_str!("../agent_detection/qwen.toml"),
        executables: &["qwen", "qwen-code"],
        packages: &["@qwen-code/qwen-code"],
    },
];

const RUNTIMES: &[&str] = &["node", "nodejs", "bun", "deno"];
// Runtime flags that run inline code or a module instead of a script.
const RUNTIME_EVAL_FLAGS: &[&str] = &["-e", "--eval", "-p", "--print", "-m"];
const RUNTIME_FLAGS_WITH_VALUE: &[&str] = &[
    "-r",
    "--require",
    "--import",
    "--loader",
    "--experimental-loader",
    "--inspect-port",
];

impl ScreenAgent {
    pub fn id(self) -> &'static str {
        AGENTS[self.0].id
    }

    pub fn display_name(self) -> &'static str {
        AGENTS[self.0].display_name
    }

    /// `screen` is the bottom screenful of the terminal; `title` is its OSC title.
    pub fn detect(self, screen: &str, title: &str) -> ScreenDetection {
        let Some(rules) = COMPILED_RULES.get(self.0).and_then(|rules| rules.as_ref()) else {
            return ScreenDetection {
                state: ScreenAgentState::Idle,
                visible: false,
            };
        };
        let mut matched: Option<&CompiledRule> = None;
        for rule in rules {
            let text = region(&rule.region, screen, title);
            if !rule.gate.matches(text, &text.to_lowercase()) {
                continue;
            }
            if matched.is_none_or(|previous| previous.priority < rule.priority) {
                matched = Some(rule);
            }
        }
        match matched {
            Some(rule) => ScreenDetection {
                state: rule.state,
                visible: rule.visible,
            },
            None => ScreenDetection {
                state: ScreenAgentState::Idle,
                visible: false,
            },
        }
    }
}

/// Finds the agent a terminal's foreground process runs, looking through `node`/`bun`
/// launchers to the script they run.
pub fn identify_screen_agent(process_name: &str, argv: &[String]) -> Option<ScreenAgent> {
    let candidates = [Some(process_name), argv.first().map(String::as_str)];
    for candidate in candidates.into_iter().flatten() {
        let name = executable_name(candidate);
        if let Some(agent) = agent_for_executable(&name) {
            return Some(agent);
        }
        if RUNTIMES.contains(&name.as_str()) {
            return runtime_script(argv).and_then(agent_for_script);
        }
    }
    None
}

fn executable_name(path: &str) -> String {
    let file_name = Path::new(path.trim_matches(['"', '\'']))
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_lowercase();
    [".exe", ".cmd", ".js", ".mjs", ".cjs"]
        .iter()
        .find_map(|extension| file_name.strip_suffix(extension))
        .map(str::to_string)
        .unwrap_or(file_name)
}

fn agent_for_executable(name: &str) -> Option<ScreenAgent> {
    AGENTS
        .iter()
        .position(|agent| agent.executables.contains(&name))
        .map(ScreenAgent)
}

fn runtime_script(argv: &[String]) -> Option<&str> {
    let mut arguments = argv.iter().skip(1);
    while let Some(argument) = arguments.next() {
        if argument == "--" {
            return arguments.next().map(String::as_str);
        }
        if RUNTIME_EVAL_FLAGS.contains(&argument.as_str()) {
            return None;
        }
        if argument.starts_with('-') {
            if RUNTIME_FLAGS_WITH_VALUE.contains(&argument.as_str()) {
                arguments.next();
            }
            continue;
        }
        return Some(argument);
    }
    None
}

fn agent_for_script(script: &str) -> Option<ScreenAgent> {
    agent_for_executable(&executable_name(script)).or_else(|| {
        let script = script.replace('\\', "/").to_lowercase();
        AGENTS
            .iter()
            .position(|agent| {
                agent
                    .packages
                    .iter()
                    .any(|package| script.contains(&format!("node_modules/{package}/")))
            })
            .map(ScreenAgent)
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    #[allow(dead_code)]
    id: String,
    #[allow(dead_code)]
    version: String,
    #[allow(dead_code)]
    min_engine_version: u32,
    #[allow(dead_code)]
    updated_at: String,
    #[serde(default)]
    #[allow(dead_code)]
    aliases: Vec<String>,
    rules: Vec<ManifestRule>,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ManifestState {
    Idle,
    Working,
    Blocked,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestRule {
    #[allow(dead_code)]
    id: String,
    state: ManifestState,
    #[serde(default)]
    priority: i32,
    #[serde(default = "default_region")]
    region: String,
    #[serde(default)]
    visible_idle: bool,
    #[serde(default)]
    visible_blocker: bool,
    #[serde(default)]
    visible_working: bool,
    #[serde(flatten)]
    gate: ManifestGate,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestGate {
    #[serde(default)]
    all: Vec<ManifestGate>,
    #[serde(default)]
    any: Vec<ManifestGate>,
    #[serde(default, rename = "not")]
    not_gate: Vec<ManifestGate>,
    #[serde(default)]
    contains: Vec<String>,
    #[serde(default)]
    regex: Vec<String>,
    #[serde(default)]
    line_regex: Vec<String>,
}

fn default_region() -> String {
    "whole_recent".to_string()
}

struct CompiledRule {
    state: ScreenAgentState,
    priority: i32,
    region: String,
    visible: bool,
    gate: CompiledGate,
}

struct CompiledGate {
    all: Vec<CompiledGate>,
    any: Vec<CompiledGate>,
    not_gate: Vec<CompiledGate>,
    contains: Vec<String>,
    regex: Vec<Regex>,
    line_regex: Vec<Regex>,
}

impl CompiledGate {
    fn matches(&self, text: &str, lowercase_text: &str) -> bool {
        self.contains
            .iter()
            .all(|needle| lowercase_text.contains(needle))
            && self.regex.iter().all(|regex| regex.is_match(text))
            && self
                .line_regex
                .iter()
                .all(|regex| text.lines().any(|line| regex.is_match(line)))
            && self
                .all
                .iter()
                .all(|gate| gate.matches(text, lowercase_text))
            && (self.any.is_empty()
                || self
                    .any
                    .iter()
                    .any(|gate| gate.matches(text, lowercase_text)))
            && !self
                .not_gate
                .iter()
                .any(|gate| gate.matches(text, lowercase_text))
    }
}

// A manifest that fails to load leaves its agent detected but always idle, rather
// than taking the others down with it.
static COMPILED_RULES: LazyLock<Vec<Option<Vec<CompiledRule>>>> = LazyLock::new(|| {
    AGENTS
        .iter()
        .map(|agent| match compile_manifest(agent.manifest) {
            Ok(rules) => Some(rules),
            Err(error) => {
                log::error!("invalid agent detection rules for {}: {error}", agent.id);
                None
            }
        })
        .collect()
});

fn compile_manifest(source: &str) -> Result<Vec<CompiledRule>, String> {
    let manifest: Manifest = toml::from_str(source).map_err(|error| error.to_string())?;
    manifest
        .rules
        .into_iter()
        .map(|rule| {
            if !region_is_supported(&rule.region) {
                return Err(format!("unsupported region `{}`", rule.region));
            }
            let (state, visible) = match rule.state {
                ManifestState::Idle => (ScreenAgentState::Idle, rule.visible_idle),
                ManifestState::Working => (ScreenAgentState::Working, rule.visible_working),
                ManifestState::Blocked => (ScreenAgentState::Blocked, rule.visible_blocker),
            };
            Ok(CompiledRule {
                state,
                priority: rule.priority,
                region: rule.region,
                visible,
                gate: compile_gate(rule.gate)?,
            })
        })
        .collect()
}

fn compile_gate(gate: ManifestGate) -> Result<CompiledGate, String> {
    let compile_regexes = |patterns: Vec<String>| {
        patterns
            .iter()
            .map(|pattern| Regex::new(pattern).map_err(|error| error.to_string()))
            .collect::<Result<Vec<_>, _>>()
    };
    Ok(CompiledGate {
        all: gate
            .all
            .into_iter()
            .map(compile_gate)
            .collect::<Result<_, _>>()?,
        any: gate
            .any
            .into_iter()
            .map(compile_gate)
            .collect::<Result<_, _>>()?,
        not_gate: gate
            .not_gate
            .into_iter()
            .map(compile_gate)
            .collect::<Result<_, _>>()?,
        contains: gate
            .contains
            .iter()
            .map(|needle| needle.to_lowercase())
            .collect(),
        regex: compile_regexes(gate.regex)?,
        line_regex: compile_regexes(gate.line_regex)?,
    })
}

fn region_is_supported(spec: &str) -> bool {
    matches!(spec, "whole_recent" | "osc_title" | "osc_progress")
        || region_line_count(spec, "bottom_lines").is_some()
        || region_line_count(spec, "bottom_non_empty_lines").is_some()
}

fn region<'a>(spec: &str, screen: &'a str, title: &'a str) -> &'a str {
    match spec {
        "whole_recent" => screen,
        "osc_title" => title,
        // Terminal progress reports (OSC 9;4) aren't tracked, so rules on them never match.
        "osc_progress" => "",
        _ => {
            if let Some(count) = region_line_count(spec, "bottom_lines") {
                bottom_lines(screen, count)
            } else if let Some(count) = region_line_count(spec, "bottom_non_empty_lines") {
                bottom_non_empty_lines(screen, count)
            } else {
                ""
            }
        }
    }
}

fn region_line_count(spec: &str, name: &str) -> Option<usize> {
    spec.strip_prefix(name)?
        .strip_prefix('(')?
        .strip_suffix(')')?
        .parse()
        .ok()
}

fn bottom_lines(text: &str, count: usize) -> &str {
    let lines = text.lines().collect::<Vec<_>>();
    suffix_from_line(text, &lines, lines.len().saturating_sub(count))
}

fn bottom_non_empty_lines(text: &str, count: usize) -> &str {
    let lines = text.lines().collect::<Vec<_>>();
    let Some(start) = lines
        .iter()
        .enumerate()
        .rev()
        .filter(|(_, line)| !line.trim().is_empty())
        .take(count)
        .last()
        .map(|(index, _)| index)
    else {
        return "";
    };
    suffix_from_line(text, &lines, start)
}

/// The rest of `text` from line `start` on. `lines` borrow from `text`, so a line's offset
/// is its distance from the start of `text`.
fn suffix_from_line<'a>(text: &'a str, lines: &[&'a str], start: usize) -> &'a str {
    match lines.get(start) {
        Some(line) => &text[line.as_ptr() as usize - text.as_ptr() as usize..],
        None => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(id: &str) -> ScreenAgent {
        AGENTS
            .iter()
            .position(|agent| agent.id == id)
            .map(ScreenAgent)
            .unwrap_or_else(|| panic!("no agent {id}"))
    }

    fn arguments(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn every_bundled_manifest_compiles() {
        for agent in AGENTS {
            if let Err(error) = compile_manifest(agent.manifest) {
                panic!("{}: {error}", agent.id);
            }
        }
    }

    #[test]
    fn agents_are_identified_by_their_executable() {
        assert_eq!(identify_screen_agent("gemini", &[]), Some(agent("gemini")));
        assert_eq!(
            identify_screen_agent("opencode", &arguments(&["/usr/local/bin/opencode"])),
            Some(agent("opencode"))
        );
        assert_eq!(identify_screen_agent("zsh", &arguments(&["-zsh"])), None);
        // Hooked agents are left to their hooks.
        assert_eq!(identify_screen_agent("claude", &[]), None);
        assert_eq!(identify_screen_agent("codex", &[]), None);
    }

    #[test]
    fn agents_launched_through_node_are_identified_by_their_script() {
        assert_eq!(
            identify_screen_agent(
                "node",
                &arguments(&["node", "--no-warnings", "/opt/homebrew/bin/gemini", "-y"])
            ),
            Some(agent("gemini"))
        );
        assert_eq!(
            identify_screen_agent(
                "node",
                &arguments(&[
                    "node",
                    "-r",
                    "preload.js",
                    "/usr/lib/node_modules/@qwen-code/qwen-code/dist/index.js"
                ])
            ),
            Some(agent("qwen"))
        );
        assert_eq!(
            identify_screen_agent("node", &arguments(&["node", "-e", "gemini"])),
            None
        );
        assert_eq!(
            identify_screen_agent("node", &arguments(&["node", "server.js"])),
            None
        );
    }

    #[test]
    fn the_highest_priority_matching_rule_wins() {
        let gemini = agent("gemini");
        assert_eq!(
            gemini.detect("Thinking... (esc to cancel, 3s)", ""),
            ScreenDetection {
                state: ScreenAgentState::Working,
                visible: true
            }
        );
        assert_eq!(
            gemini
                .detect(
                    "│ Allow execution of: 'ls'?\n│ ● 1. Yes, allow once\n(esc to cancel)",
                    ""
                )
                .state,
            ScreenAgentState::Blocked
        );
        assert_eq!(
            gemini.detect("> Type your message", ""),
            ScreenDetection {
                state: ScreenAgentState::Idle,
                visible: false
            }
        );
    }

    #[test]
    fn title_rules_read_the_terminal_title() {
        let amp = agent("amp");
        assert_eq!(
            amp.detect("", "⠋ fix tests").state,
            ScreenAgentState::Working
        );
        assert_eq!(
            amp.detect("", "superzent - amp - main"),
            ScreenDetection {
                state: ScreenAgentState::Idle,
                visible: true
            }
        );
    }

    #[test]
    fn bottom_regions_count_from_the_last_lines() {
        let text = "one\n\ntwo\nthree\n\n";
        assert_eq!(bottom_non_empty_lines(text, 2), "two\nthree\n\n");
        assert_eq!(bottom_non_empty_lines(text, 9), text);
        assert_eq!(bottom_lines(text, 2), "three\n\n");
        assert_eq!(bottom_non_empty_lines("", 2), "");
    }
}
