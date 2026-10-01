//! `agent read`: an agent's last full reply, or the bottom of its screen.

use super::args::{Args, Spec};
use super::env::{AgentTmux, RealTmux};
use super::{EXIT_ERROR, EXIT_OK, target, usage_error};
use crate::activity::AgentResponse;

const DEFAULT_SCREEN_LINES: u64 = 40;

/// The stored reply, unless it belongs to an earlier session in the same
/// pane (`/clear`, or a different agent started there since).
pub(super) fn current_reply(
    response: Option<AgentResponse>,
    pane_session_id: &str,
) -> Result<AgentResponse, String> {
    let response = response.ok_or("no reply recorded yet")?;
    if !pane_session_id.is_empty()
        && !response.session_id.is_empty()
        && response.session_id != pane_session_id
    {
        return Err("the recorded reply belongs to an earlier session".into());
    }
    Ok(response)
}

pub(super) fn run(raw: &[String]) -> i32 {
    const SPEC: Spec = Spec {
        values: &["lines"],
        switches: &["response", "screen", "json"],
    };
    let parsed = match Args::parse(raw, &SPEC) {
        Ok(p) => p,
        Err(e) => return usage_error(&e),
    };
    let [query] = parsed.positionals.as_slice() else {
        return usage_error("read takes exactly one <target>");
    };
    if parsed.has("screen") && parsed.has("response") {
        return usage_error("pick one of --response and --screen");
    }
    let lines = match parsed.number("lines") {
        Ok(n) => n.unwrap_or(DEFAULT_SCREEN_LINES).max(1) as usize,
        Err(e) => return usage_error(&e),
    };
    let loc = match target::lookup(query) {
        Ok(loc) => loc,
        Err(e) => {
            eprintln!("error: {e}");
            return EXIT_ERROR;
        }
    };

    if parsed.has("screen") {
        return match RealTmux.capture(&loc.pane_id, lines) {
            Ok(screen) => {
                println!("{screen}");
                EXIT_OK
            }
            Err(e) => {
                eprintln!("error: {e}");
                EXIT_ERROR
            }
        };
    }

    let reply = current_reply(
        crate::activity::read_response(&loc.pane_id),
        &loc.session_id,
    );
    match reply {
        Ok(reply) if parsed.has("json") => {
            let mut json = reply.to_json();
            json["pane_id"] = loc.pane_id.into();
            println!("{json}");
            EXIT_OK
        }
        Ok(reply) => {
            println!("{}", reply.message);
            EXIT_OK
        }
        Err(e) => {
            eprintln!(
                "error: {}: {e}; try `agent read {} --screen`",
                loc.pane_id, loc.pane_id
            );
            EXIT_ERROR
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(session: &str) -> AgentResponse {
        AgentResponse {
            session_id: session.into(),
            message: "hi".into(),
            ..Default::default()
        }
    }

    #[test]
    fn current_reply_checks_the_session() {
        assert!(current_reply(None, "a").is_err());
        assert!(current_reply(Some(reply("a")), "a").is_ok());
        assert!(current_reply(Some(reply("old")), "a").is_err());
        // Missing ids on either side can't prove a mismatch.
        assert!(current_reply(Some(reply("")), "a").is_ok());
        assert!(current_reply(Some(reply("a")), "").is_ok());
    }
}
