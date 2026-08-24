//! The runner-side half of the operator command channel.
//!
//! The runner is unreachable from the cloud by design, so commands are never
//! pushed here: the worker queues them, and this module pulls everything
//! after the runner's own high-water id once per window. Delivery is
//! at-least-once, so every command must be idempotent to apply — flipping a
//! kill switch twice is one flip; ending a session twice is one end.
//!
//! Applying a command is deliberately boring and lives entirely outside the
//! hot loop: no ring, no engine thread, no allocation-sensitive path ever
//! learns this module exists.

use serde::Deserialize;

/// One operator command, exactly the shapes the worker's zod schema admits.
/// Unknown tags and malformed bodies fail deserialization here, which the
/// caller counts as a skipped line — a bad command must never take the
/// runner down.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "t", rename_all = "kebab-case")]
pub enum OperatorCommand {
    /// Requested session open. This paper runner owns exactly one session
    /// per process, so a start arriving mid-session is refused upstream by
    /// the caller; a production supervisor would honor it when idle.
    SessionStart { mode: String },
    /// Graceful stop through the same shutdown path as a finite run.
    SessionEnd,
    /// Arm/`engage: true` or disarm/`engage: false` the kill switch. A
    /// disarm requires an explicit `confirm: true`; a bare disarm is refused
    /// by the apply path (mirrors the worker's zod schema).
    KillSwitch {
        engage: bool,
        #[serde(default)]
        confirm: bool,
    },
}

/// Wire envelope from the pull endpoint: monotonic id plus the command.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CommandEnvelope {
    pub id: u64,
    pub command: OperatorCommand,
}

/// Derives the pull endpoint from the configured ingest URL: same origin and
/// path prefix, final segment replaced — `/api/live/ingest` becomes
/// `/api/live/commands`.
pub fn control_url_from_ingest(ingest_url: &str) -> String {
    match ingest_url.rsplit_once('/') {
        Some((prefix, _)) if !prefix.is_empty() => format!("{prefix}/commands"),
        _ => String::from("/api/live/commands"),
    }
}

/// Fetches queued commands with id greater than `after_id`, oldest first.
/// An empty queue is `Ok(vec![])`; transport failures are `Err` and the
/// caller simply retries next window.
pub fn poll_commands(
    agent: &ureq::Agent,
    url: &str,
    token: &str,
    after_id: u64,
) -> Result<Vec<CommandEnvelope>, String> {
    let mut request = agent.get(url);
    if !token.is_empty() {
        request = request.set("Authorization", &format!("Bearer {token}"));
    }
    let response = request
        .query("afterId", &after_id.to_string())
        .call()
        .map_err(|error| format!("command poll failed: {error}"))?;
    let body = response
        .into_string()
        .map_err(|error| format!("command poll body unreadable: {error}"))?;

    let mut envelopes = Vec::new();
    for line in body.lines().filter(|line| !line.trim().is_empty()) {
        match serde_json::from_str::<CommandEnvelope>(line) {
            Ok(envelope) => envelopes.push(envelope),
            Err(error) => eprintln!("[live-control] skipping unparsable command line: {error}"),
        }
    }
    envelopes.sort_by_key(|envelope| envelope.id);
    Ok(envelopes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_command_shape_the_worker_admits() {
        let kill = serde_json::from_str::<CommandEnvelope>(
            r#"{"id":7,"command":{"t":"kill-switch","engage":false,"confirm":true}}"#,
        )
        .expect("kill-switch parses");
        assert_eq!(
            kill.command,
            OperatorCommand::KillSwitch {
                engage: false,
                confirm: true
            }
        );
        assert_eq!(kill.id, 7);

        let start = serde_json::from_str::<CommandEnvelope>(
            r#"{"id":8,"command":{"t":"session-start","mode":"paper"}}"#,
        )
        .expect("session-start parses");
        assert_eq!(
            start.command,
            OperatorCommand::SessionStart {
                mode: String::from("paper")
            }
        );

        let end =
            serde_json::from_str::<CommandEnvelope>(r#"{"id":9,"command":{"t":"session-end"}}"#)
                .expect("session-end parses");
        assert_eq!(end.command, OperatorCommand::SessionEnd);
    }

    #[test]
    fn refuses_unknown_tags_and_malformed_bodies_instead_of_guessing() {
        assert!(
            serde_json::from_str::<CommandEnvelope>(r#"{"id":1,"command":{"t":"self-destruct"}}"#)
                .is_err(),
            "unknown command tag must not deserialize"
        );
        assert!(
            serde_json::from_str::<CommandEnvelope>(
                r#"{"id":2,"command":{"t":"kill-switch","engage":1}}"#
            )
            .is_err(),
            "non-boolean engage must not deserialize"
        );
    }

    #[test]
    fn derives_pull_endpoint_from_any_ingest_path() {
        assert_eq!(
            control_url_from_ingest("http://127.0.0.1:8787/api/live/ingest"),
            "http://127.0.0.1:8787/api/live/commands"
        );
        assert_eq!(
            control_url_from_ingest("https://dash.example/api/live/ingest"),
            "https://dash.example/api/live/commands"
        );
    }
}
