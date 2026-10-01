use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::process::Command;

use crate::tmux::AgentType;

#[derive(Debug, Clone)]
pub(crate) struct ProcessInfo {
    pub(crate) comm: String,
    pub(crate) args: String,
}

#[derive(Debug, Clone)]
pub(crate) struct ProcessSnapshot {
    pub(crate) children_of: HashMap<u32, Vec<u32>>,
    pub(crate) info_by_pid: HashMap<u32, ProcessInfo>,
}

impl ProcessSnapshot {
    pub(crate) fn scan() -> Option<Self> {
        let output = Command::new("ps")
            .args(["-eo", "pid=,ppid=,comm=,args="])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        Some(Self::from_ps_output(&String::from_utf8_lossy(
            &output.stdout,
        )))
    }

    pub(crate) fn from_ps_output(ps_output: &str) -> Self {
        let mut children_of: HashMap<u32, Vec<u32>> = HashMap::new();
        let mut info_by_pid: HashMap<u32, ProcessInfo> = HashMap::new();

        for line in ps_output.lines() {
            let mut parts = line.split_whitespace();
            let Some(pid_str) = parts.next() else {
                continue;
            };
            let Some(ppid_str) = parts.next() else {
                continue;
            };
            let Ok(pid) = pid_str.parse::<u32>() else {
                continue;
            };
            let Ok(ppid) = ppid_str.parse::<u32>() else {
                continue;
            };
            let Some(comm) = parts.next() else {
                continue;
            };

            children_of.entry(ppid).or_default().push(pid);
            info_by_pid.insert(
                pid,
                ProcessInfo {
                    comm: comm.to_string(),
                    args: parts.collect::<Vec<_>>().join(" "),
                },
            );
        }

        Self {
            children_of,
            info_by_pid,
        }
    }

    pub(crate) fn descendants(&self, seed_pids: &[u32]) -> HashSet<u32> {
        let mut seen = HashSet::new();
        let mut queue: VecDeque<u32> = seed_pids.iter().copied().collect();

        while let Some(pid) = queue.pop_front() {
            if !seen.insert(pid) {
                continue;
            }
            if let Some(children) = self.children_of.get(&pid) {
                for &child in children {
                    if !seen.contains(&child) {
                        queue.push_back(child);
                    }
                }
            }
        }

        seen
    }

    pub(crate) fn tree_has_agent(&self, seed_pids: &[u32], agent: &AgentType) -> bool {
        let agent_name = agent.as_str();
        self.descendants(seed_pids).into_iter().any(|pid| {
            self.info_by_pid
                .get(&pid)
                .map(|info| process_matches_agent(info, agent_name))
                .unwrap_or(false)
        })
    }

    pub(crate) fn command_lines_for_tree(&self, seed_pids: &[u32]) -> Vec<String> {
        self.descendants(seed_pids)
            .into_iter()
            .filter_map(|pid| self.info_by_pid.get(&pid))
            .map(|info| {
                if info.args.is_empty() {
                    info.comm.clone()
                } else {
                    info.args.trim().to_string()
                }
            })
            .collect()
    }
}

impl ProcessSnapshot {
    /// Pid of the agent when it is the pane's interactive program: the
    /// pane root itself, a direct child of the pane shell, or the child of
    /// a launcher shim (`bun …/codex.js` spawning the native binary).
    /// Agents nested deeper, e.g. `bash worker.sh` → `claude -p`, belong
    /// to some script and must not be treated as resumable sessions.
    pub(crate) fn interactive_agent_pid(&self, pane_pid: u32, agent_name: &str) -> Option<u32> {
        let matches = |pid: &u32| {
            self.info_by_pid
                .get(pid)
                .is_some_and(|info| process_matches_agent(info, agent_name))
        };
        if matches(&pane_pid) {
            return Some(pane_pid);
        }
        let children = self.children_of.get(&pane_pid)?;
        if let Some(&pid) = children.iter().find(|pid| matches(pid)) {
            return Some(pid);
        }
        children.iter().find_map(|shim| {
            let info = self.info_by_pid.get(shim)?;
            if !is_launcher_shim(info, agent_name) {
                return None;
            }
            let launched = self
                .children_of
                .get(shim)
                .and_then(|kids| kids.iter().copied().find(|pid| matches(pid)));
            Some(launched.unwrap_or(*shim))
        })
    }
}

/// `node`/`bun` running a script named after the agent (`codex.js`,
/// `bin/opencode`). Only the script slot counts, so a watcher like
/// `node watch.mjs … claude` is not mistaken for the agent.
fn is_launcher_shim(info: &ProcessInfo, agent_name: &str) -> bool {
    let mut tokens = info.args.split_whitespace();
    let Some(interpreter) = tokens.next() else {
        return false;
    };
    let interpreter = command_basename(interpreter);
    if !matches!(interpreter, "node" | "bun" | "deno") {
        return false;
    }
    tokens
        .find(|t| !t.starts_with('-'))
        .is_some_and(|script| script_stem(script) == agent_name)
}

fn script_stem(token: &str) -> &str {
    let base = command_basename(token.trim_matches('"'));
    [".js", ".mjs", ".cjs", ".ts"]
        .iter()
        .find_map(|ext| base.strip_suffix(ext))
        .unwrap_or(base)
}

/// Environment variables `read_invocation` may copy from an agent process.
/// Everything else in `environ` (API keys, tokens) is never read into
/// memory beyond the filter, never stored, and never printed.
pub(crate) const ENV_ALLOWLIST: &[&str] = &["CLAUDE_CONFIG_DIR", "CODEX_HOME", "XDG_DATA_HOME"];

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Invocation {
    pub(crate) argv: Vec<String>,
    /// Allowlisted variables only (see [`ENV_ALLOWLIST`]).
    pub(crate) env: Vec<(String, String)>,
    /// False when argv came from whitespace-splitting `ps` output, which
    /// cannot tell an argument containing spaces from two arguments.
    pub(crate) argv_exact: bool,
}

/// Exact argv plus allowlisted env of a running process.
pub(crate) fn read_invocation(pid: u32) -> Option<Invocation> {
    #[cfg(target_os = "linux")]
    {
        let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
        let environ = std::fs::read(format!("/proc/{pid}/environ")).unwrap_or_default();
        let argv = split_nul(&cmdline);
        if argv.is_empty() {
            return None;
        }
        Some(Invocation {
            argv,
            env: filter_env(&environ),
            argv_exact: true,
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        let output = Command::new("ps")
            .args(["-o", "args=", "-p", &pid.to_string()])
            .output()
            .ok()?;
        let args = String::from_utf8_lossy(&output.stdout);
        let argv: Vec<String> = args.split_whitespace().map(str::to_string).collect();
        if argv.is_empty() {
            return None;
        }
        Some(Invocation {
            argv,
            env: Vec::new(),
            argv_exact: false,
        })
    }
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn split_nul(bytes: &[u8]) -> Vec<String> {
    bytes
        .split(|b| *b == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8_lossy(part).into_owned())
        .collect()
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn filter_env(environ: &[u8]) -> Vec<(String, String)> {
    environ
        .split(|b| *b == 0)
        .filter_map(|entry| {
            let entry = std::str::from_utf8(entry).ok()?;
            let (key, value) = entry.split_once('=')?;
            ENV_ALLOWLIST
                .contains(&key)
                .then(|| (key.to_string(), value.to_string()))
        })
        .collect()
}

/// Rewrite a raw agent argv so it starts with the bare agent name:
/// `/…/vendor/…/codex --yolo` and `bun /…/codex.js --yolo` both become
/// `codex --yolo`. When no token names the agent (a process that rewrote
/// its title), only the agent name is kept since the flags are unknown.
pub(crate) fn normalize_agent_argv(argv: &[String], agent_name: &str) -> Vec<String> {
    match argv.iter().position(|t| script_stem(t) == agent_name) {
        Some(i) => std::iter::once(agent_name.to_string())
            .chain(argv[i + 1..].iter().cloned())
            .collect(),
        None => vec![agent_name.to_string()],
    }
}

pub(crate) fn command_basename(command: &str) -> &str {
    Path::new(command)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(command)
}

pub(crate) fn process_matches_agent(info: &ProcessInfo, agent_name: &str) -> bool {
    if command_basename(&info.comm) == agent_name {
        return true;
    }

    let Some(command) = info.args.split_whitespace().next() else {
        return false;
    };
    command_basename(command.trim_matches('"')) == agent_name
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descendants_walks_process_tree() {
        let snapshot = ProcessSnapshot {
            children_of: HashMap::from([(1, vec![2, 3]), (2, vec![4]), (4, vec![5])]),
            info_by_pid: HashMap::new(),
        };
        let seen = snapshot.descendants(&[1]);
        assert!(seen.contains(&1));
        assert!(seen.contains(&2));
        assert!(seen.contains(&3));
        assert!(seen.contains(&4));
        assert!(seen.contains(&5));
    }

    #[test]
    fn parse_ps_processes_preserves_spaced_args() {
        let snapshot = ProcessSnapshot::from_ps_output(
            "100 1 codex /Applications/Codex App/bin/codex --full-auto\n101 100 sh sh -c wrapper\n",
        );

        assert_eq!(snapshot.children_of.get(&1).cloned(), Some(vec![100]));
        let info = snapshot.info_by_pid.get(&100).expect("process info");
        assert_eq!(info.comm, "codex");
        assert_eq!(info.args, "/Applications/Codex App/bin/codex --full-auto");
    }

    #[test]
    fn tree_has_agent_matches_descendant_process_name() {
        let snapshot = ProcessSnapshot::from_ps_output(
            "100 1 fish fish -c opencode\n101 100 opencode opencode\n",
        );

        assert!(snapshot.tree_has_agent(&[100], &AgentType::OpenCode));
        assert!(!snapshot.tree_has_agent(&[100], &AgentType::Codex));
    }

    #[test]
    fn interactive_agent_pid_accepts_shell_child() {
        let snapshot = ProcessSnapshot::from_ps_output(
            "100 1 zsh -zsh\n101 100 claude claude --chrome\n102 101 uv uv tool uvx mcp\n",
        );
        assert_eq!(snapshot.interactive_agent_pid(100, "claude"), Some(101));
        assert_eq!(snapshot.interactive_agent_pid(100, "codex"), None);
    }

    #[test]
    fn interactive_agent_pid_accepts_pane_root() {
        let snapshot = ProcessSnapshot::from_ps_output("100 1 claude claude\n");
        assert_eq!(snapshot.interactive_agent_pid(100, "claude"), Some(100));
    }

    #[test]
    fn interactive_agent_pid_follows_launcher_shim() {
        let snapshot = ProcessSnapshot::from_ps_output(
            "100 1 zsh -zsh\n\
             101 100 bun bun /home/u/.bun/install/global/node_modules/@openai/codex/bin/codex.js --yolo\n\
             102 101 codex /home/u/vendor/x86_64-unknown-linux-musl/bin/codex --yolo\n",
        );
        assert_eq!(snapshot.interactive_agent_pid(100, "codex"), Some(102));
    }

    #[test]
    fn interactive_agent_pid_skips_agents_nested_in_scripts() {
        // debit-loop style pane: the agent runs under worker.sh and a
        // watcher whose args merely mention it.
        let snapshot = ProcessSnapshot::from_ps_output(
            "100 1 bash bash\n\
             101 100 bash bash /loop/worker.sh /jobs/fix-550\n\
             102 100 node-MainThread node /loop/watch.mjs /jobs/fix-550 101 200000 claude\n\
             103 101 timeout timeout --foreground 2h claude -p fix it\n\
             104 103 claude claude -p fix it\n",
        );
        assert_eq!(snapshot.interactive_agent_pid(100, "claude"), None);
    }

    #[test]
    fn filter_env_keeps_only_allowlisted_keys() {
        let environ = b"HOME=/home/u\0ANTHROPIC_API_KEY=secret\0CLAUDE_CONFIG_DIR=/home/u/.claude-x\0\0XDG_DATA_HOME=/d\0";
        assert_eq!(
            filter_env(environ),
            vec![
                (
                    "CLAUDE_CONFIG_DIR".to_string(),
                    "/home/u/.claude-x".to_string()
                ),
                ("XDG_DATA_HOME".to_string(), "/d".to_string()),
            ]
        );
    }

    #[test]
    fn split_nul_drops_trailing_terminator() {
        assert_eq!(
            split_nul(b"claude\0--append-system-prompt\0two words\0"),
            vec!["claude", "--append-system-prompt", "two words"]
        );
    }

    #[test]
    fn normalize_agent_argv_strips_paths_and_shims() {
        let argv = |s: &str| s.split(' ').map(str::to_string).collect::<Vec<_>>();
        assert_eq!(
            normalize_agent_argv(&argv("/v/bin/codex --yolo"), "codex"),
            argv("codex --yolo")
        );
        assert_eq!(
            normalize_agent_argv(&argv("bun /g/codex.js -m o3"), "codex"),
            argv("codex -m o3")
        );
        assert_eq!(
            normalize_agent_argv(&argv("claude --chrome"), "claude"),
            argv("claude --chrome")
        );
        assert_eq!(
            normalize_agent_argv(&argv("node-title --x"), "opencode"),
            argv("opencode")
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn read_invocation_reads_own_process() {
        let inv = read_invocation(std::process::id()).expect("own invocation");
        assert!(inv.argv_exact);
        assert!(!inv.argv.is_empty());
        assert!(
            inv.env
                .iter()
                .all(|(k, _)| ENV_ALLOWLIST.contains(&k.as_str()))
        );
    }

    #[test]
    fn process_matches_agent_requires_command_name_match() {
        assert!(process_matches_agent(
            &ProcessInfo {
                comm: "claude".to_string(),
                args: "/opt/homebrew/bin/claude --flag".to_string(),
            },
            "claude",
        ));
        assert!(process_matches_agent(
            &ProcessInfo {
                comm: "node".to_string(),
                args: "/usr/local/bin/opencode".to_string(),
            },
            "opencode",
        ));
        assert!(!process_matches_agent(
            &ProcessInfo {
                comm: "not-opencode".to_string(),
                args: "/usr/local/bin/not-opencode".to_string(),
            },
            "opencode",
        ));
    }
}
