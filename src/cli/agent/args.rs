//! Minimal long-option parser for the `agent` subcommands: `--flag value`,
//! `--flag=value`, boolean switches, positionals, and everything after
//! `--` kept verbatim.

use std::collections::HashMap;

pub(super) struct Spec {
    /// Options that take a value.
    pub(super) values: &'static [&'static str],
    /// Boolean switches.
    pub(super) switches: &'static [&'static str],
}

#[derive(Debug, Default)]
pub(super) struct Args {
    pub(super) positionals: Vec<String>,
    values: HashMap<&'static str, String>,
    switches: Vec<&'static str>,
    /// Arguments after a bare `--`, untouched.
    pub(super) trailing: Vec<String>,
}

impl Args {
    pub(super) fn parse(raw: &[String], spec: &Spec) -> Result<Self, String> {
        let mut out = Args::default();
        let mut it = raw.iter();
        while let Some(arg) = it.next() {
            if arg == "--" {
                out.trailing = it.cloned().collect();
                break;
            }
            let Some(body) = arg.strip_prefix("--") else {
                if arg.len() > 1 && arg.starts_with('-') {
                    return Err(format!("unknown option {arg}"));
                }
                out.positionals.push(arg.clone());
                continue;
            };
            let (name, inline) = match body.split_once('=') {
                Some((name, value)) => (name, Some(value.to_string())),
                None => (body, None),
            };
            if let Some(&key) = spec.values.iter().find(|k| **k == name) {
                let value = match inline {
                    Some(v) => v,
                    None => it
                        .next()
                        .cloned()
                        .ok_or_else(|| format!("--{name} needs a value"))?,
                };
                out.values.insert(key, value);
            } else if let Some(&key) = spec.switches.iter().find(|k| **k == name) {
                if inline.is_some() {
                    return Err(format!("--{name} takes no value"));
                }
                out.switches.push(key);
            } else {
                return Err(format!("unknown option --{name}"));
            }
        }
        Ok(out)
    }

    pub(super) fn value(&self, name: &str) -> Option<&str> {
        self.values.get(name).map(String::as_str)
    }

    pub(super) fn has(&self, name: &str) -> bool {
        self.switches.contains(&name)
    }

    pub(super) fn number(&self, name: &str) -> Result<Option<u64>, String> {
        self.value(name)
            .map(|v| {
                v.parse()
                    .map_err(|_| format!("--{name} expects a whole number, got `{v}`"))
            })
            .transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC: Spec = Spec {
        values: &["timeout", "agent"],
        switches: &["wait", "json"],
    };

    fn parse(raw: &[&str]) -> Result<Args, String> {
        let raw: Vec<String> = raw.iter().map(|s| s.to_string()).collect();
        Args::parse(&raw, &SPEC)
    }

    #[test]
    fn parses_values_switches_and_positionals() {
        let a = parse(&[
            "%3",
            "--timeout",
            "5",
            "--wait",
            "hello world",
            "--agent=codex",
        ])
        .unwrap();
        assert_eq!(a.positionals, vec!["%3", "hello world"]);
        assert_eq!(a.value("timeout"), Some("5"));
        assert_eq!(a.value("agent"), Some("codex"));
        assert!(a.has("wait"));
        assert!(!a.has("json"));
        assert_eq!(a.number("timeout").unwrap(), Some(5));
    }

    #[test]
    fn keeps_everything_after_double_dash() {
        let a = parse(&["--wait", "--", "--model", "o3", "-x"]).unwrap();
        assert_eq!(a.trailing, vec!["--model", "o3", "-x"]);
        assert!(a.positionals.is_empty());
    }

    #[test]
    fn lone_dash_is_a_positional() {
        assert_eq!(parse(&["%1", "-"]).unwrap().positionals, vec!["%1", "-"]);
    }

    #[test]
    fn rejects_unknown_and_malformed_options() {
        assert!(parse(&["--nope"]).is_err());
        assert!(parse(&["-x"]).is_err());
        assert!(parse(&["--timeout"]).is_err());
        assert!(parse(&["--wait=1"]).is_err());
        assert!(
            parse(&["--timeout", "soon"])
                .unwrap()
                .number("timeout")
                .is_err()
        );
    }
}
