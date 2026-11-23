use crate::db::{now_ms, Db, Workflow, WorkflowStep};
use crate::runtime::{Runtime, TurnOutcome};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter};

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowProgress {
    pub workflow_id: String,
    pub step: usize,
    pub total: usize,
    pub agent_id: Option<String>,
    pub state: &'static str,
    pub error: Option<String>,
}

pub fn step_prompt(template: &str, input: &str, previous: Option<&str>) -> String {
    let previous_text = previous.unwrap_or(input);
    let mut prompt = template.replace("{{input}}", input).replace("{{previous}}", previous_text);
    if let Some(previous) = previous.filter(|p| !p.trim().is_empty()) {
        if !template.contains("{{previous}}") {
            prompt = format!("{prompt}\n\nResult from the previous step:\n{previous}");
        }
    }
    prompt.trim().to_string()
}

pub fn validate(workflow: &Workflow, agent_exists: impl Fn(&str) -> bool) -> Result<(), String> {
    if workflow.name.trim().is_empty() {
        return Err("Give the spell a name.".into());
    }
    if workflow.steps.is_empty() {
        return Err("Add at least one step.".into());
    }
    for (index, step) in workflow.steps.iter().enumerate() {
        if !agent_exists(&step.agent_id) {
            return Err(format!("Step {} needs a familiar.", index + 1));
        }
        if step.prompt.trim().is_empty() {
            return Err(format!("Step {} needs instructions.", index + 1));
        }
    }
    Ok(())
}

/// Creates or updates a workflow after trimming and validating it. `created_by` applies only to new
/// workflows; an edit keeps the original creator.
pub fn save(db: &Db, id: Option<&str>, name: &str, steps: Vec<WorkflowStep>, created_by: Option<String>) -> Result<Workflow, String> {
    let existing = match id {
        Some(id) => db.list_workflows()?.into_iter().find(|w| w.id == id),
        None => None,
    };
    let workflow = Workflow {
        id: existing.as_ref().map(|w| w.id.clone()).unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        name: name.trim().to_string(),
        steps: steps.into_iter().map(|s| WorkflowStep { agent_id: s.agent_id, prompt: s.prompt.trim().to_string() }).collect(),
        created_at: existing.as_ref().map(|w| w.created_at).unwrap_or_else(now_ms),
        created_by: match existing {
            Some(w) => w.created_by,
            None => created_by,
        },
    };
    let agents = db.list_agents()?;
    validate(&workflow, |id| agents.iter().any(|a| a.id == id))?;
    db.save_workflow(&workflow)?;
    Ok(workflow)
}

pub struct WorkflowRunner {
    db: Arc<Db>,
    runtime: Arc<Runtime>,
    app: AppHandle,
    running: Mutex<HashMap<String, Option<String>>>,
}

impl WorkflowRunner {
    pub fn new(db: Arc<Db>, runtime: Arc<Runtime>, app: AppHandle) -> Arc<Self> {
        Arc::new(WorkflowRunner { db, runtime, app, running: Mutex::new(HashMap::new()) })
    }

    pub fn running(&self) -> Vec<String> {
        self.running.lock().map(|r| r.keys().cloned().collect()).unwrap_or_default()
    }

    pub fn start(self: &Arc<Self>, workflow_id: &str, input: String) -> Result<(), String> {
        let workflow = self
            .db
            .list_workflows()?
            .into_iter()
            .find(|w| w.id == workflow_id)
            .ok_or("That spell no longer exists.")?;
        {
            let mut running = self.running.lock().map_err(|_| "workflow state poisoned")?;
            if running.contains_key(workflow_id) {
                return Err("This spell is already being cast.".into());
            }
            running.insert(workflow_id.to_string(), None);
        }
        let runner = self.clone();
        tauri::async_runtime::spawn(async move { runner.run(workflow, input).await });
        Ok(())
    }

    pub async fn stop(&self, workflow_id: &str) -> Result<(), String> {
        let agent = self.running.lock().ok().and_then(|r| r.get(workflow_id).cloned().flatten());
        match agent {
            Some(agent_id) => self.runtime.stop(&agent_id).await,
            None => Ok(()),
        }
    }

    fn emit(&self, progress: WorkflowProgress) {
        let _ = self.app.emit("workflow_progress", progress);
    }

    async fn run(self: Arc<Self>, workflow: Workflow, input: String) {
        let total = workflow.steps.len();
        let mut previous: Option<String> = None;
        let source = format!("{}{}", crate::runtime::SPELL_SOURCE, workflow.name);
        for (index, step) in workflow.steps.iter().enumerate() {
            if let Ok(mut running) = self.running.lock() {
                running.insert(workflow.id.clone(), Some(step.agent_id.clone()));
            }
            self.emit(WorkflowProgress {
                workflow_id: workflow.id.clone(),
                step: index + 1,
                total,
                agent_id: Some(step.agent_id.clone()),
                state: "running",
                error: None,
            });
            let prompt = step_prompt(&step.prompt, &input, previous.as_deref());
            let outcome = self.runtime.send_and_wait(&step.agent_id, prompt, Some(&source)).await;
            let (state, error) = match outcome {
                TurnOutcome::Done(text) => {
                    previous = Some(text);
                    continue;
                }
                TurnOutcome::Stopped => ("stopped", None),
                TurnOutcome::Failed(reason) => ("failed", Some(reason)),
            };
            crate::notify::spell_ended(&self.app, &workflow.name, state, index + 1, total, &step.agent_id, error.as_deref());
            self.finish(&workflow.id, index + 1, total, Some(step.agent_id.clone()), state, error);
            return;
        }
        if let Some(last) = workflow.steps.last() {
            crate::notify::spell_ended(&self.app, &workflow.name, "done", total, total, &last.agent_id, previous.as_deref());
        }
        self.finish(&workflow.id, total, total, None, "done", None);
    }

    fn finish(&self, id: &str, step: usize, total: usize, agent_id: Option<String>, state: &'static str, error: Option<String>) {
        if let Ok(mut running) = self.running.lock() {
            running.remove(id);
        }
        self.emit(WorkflowProgress { workflow_id: id.to_string(), step, total, agent_id, state, error });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::WorkflowStep;

    #[test]
    fn fills_placeholders_and_appends_previous_output() {
        assert_eq!(step_prompt("Research {{input}}", "solar", None), "Research solar");
        assert_eq!(step_prompt("Summarize: {{previous}}", "solar", Some("notes")), "Summarize: notes");
        assert_eq!(
            step_prompt("Write a tweet", "solar", Some("notes")),
            "Write a tweet\n\nResult from the previous step:\nnotes"
        );
        assert_eq!(step_prompt("First: {{previous}}", "the input", None), "First: the input");
    }

    #[test]
    fn validates_steps() {
        let mut workflow = Workflow {
            id: "w".into(),
            name: "Ship".into(),
            steps: vec![WorkflowStep { agent_id: "a".into(), prompt: "go".into() }],
            created_at: 0,
            created_by: None,
        };
        assert!(validate(&workflow, |id| id == "a").is_ok());
        assert!(validate(&workflow, |_| false).is_err());
        workflow.steps[0].prompt = " ".into();
        assert!(validate(&workflow, |_| true).is_err());
        workflow.steps.clear();
        assert!(validate(&workflow, |_| true).is_err());
    }
}
