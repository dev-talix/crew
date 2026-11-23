//! What a familiar is told when a chat starts, beyond its own instructions: its memory and any other
//! standing context. Each part returns None when it has nothing to say.

use crate::db::Agent;

pub fn session_prompt(agent: &Agent) -> Option<String> {
    let parts: Vec<String> = [
        crate::memory::session_prompt(agent),
        crate::ledger::session_prompt(agent),
        crate::drafts::session_prompt(agent),
    ]
    .into_iter()
    .flatten()
    .collect();
    (!parts.is_empty()).then(|| parts.join("\n\n"))
}
