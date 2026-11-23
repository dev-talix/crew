use crate::db::{now_ms, CheckWhen, Db, StepCheck, Workflow, WorkflowStep};
use crate::runtime::{Runtime, TurnOutcome};
use crate::workflow_runs;
use serde::Serialize;
use std::collections::HashMap;
use std::future::Future;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter};

/// How many times one check may send a spell to its step before the cast stops.
pub const MAX_LOOPS: u32 = 3;

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowProgress {
    pub workflow_id: String,
    pub run_id: String,
    /// The first step running now, counting from 1.
    pub step: usize,
    pub total: usize,
    pub agent_id: Option<String>,
    /// Every step running now, counting from 1: more than one for steps that run alongside each other.
    pub steps: Vec<usize>,
    /// The familiars of those steps.
    pub agents: Vec<String>,
    pub state: &'static str,
    pub error: Option<String>,
}

/// Fills `{{input}}` and `{{previous}}` in a step's prompt. When the step doesn't use `{{previous}}`, the
/// previous reply goes at the end. `several` says the previous text joins replies from steps that ran together.
pub fn step_prompt(template: &str, input: &str, previous: Option<&str>, several: bool) -> String {
    let previous_text = previous.unwrap_or(input);
    let mut prompt = template.replace("{{input}}", input).replace("{{previous}}", previous_text);
    if let Some(previous) = previous.filter(|p| !p.trim().is_empty()) {
        if !template.contains("{{previous}}") {
            let heading = if several { "Results from the previous steps" } else { "Result from the previous step" };
            prompt = format!("{prompt}\n\n{heading}:\n{previous}");
        }
    }
    prompt.trim().to_string()
}

/// The steps that start together: each group is a step and the steps after it that run alongside it.
pub fn groups(steps: &[WorkflowStep]) -> Vec<Range<usize>> {
    let mut groups: Vec<Range<usize>> = Vec::new();
    for (index, step) in steps.iter().enumerate() {
        match groups.last_mut() {
            Some(group) if step.parallel => group.end = index + 1,
            _ => groups.push(index..index + 1),
        }
    }
    groups
}

/// Joins the replies of steps that ran together, each under its familiar's name.
pub fn join_replies(replies: &[(usize, &str, &str)]) -> String {
    if let [(_, _, text)] = replies {
        return text.to_string();
    }
    replies
        .iter()
        .map(|(step, name, text)| {
            let shared = replies.iter().filter(|(_, other, _)| other == name).count() > 1;
            let label = if shared { format!("{name} (step {})", step + 1) } else { name.to_string() };
            format!("{label} replied:\n{}", text.trim())
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

impl StepCheck {
    /// Whether the reply sends the spell to `go_to`. Case doesn't matter.
    pub fn matches(&self, reply: &str) -> bool {
        let found = reply.to_lowercase().contains(&self.phrase.to_lowercase());
        match self.when {
            CheckWhen::Contains => found,
            CheckWhen::Lacks => !found,
        }
    }
}

pub fn validate(workflow: &Workflow, agent_exists: impl Fn(&str) -> bool) -> Result<(), String> {
    if workflow.name.trim().is_empty() {
        return Err("Give the spell a name.".into());
    }
    if workflow.steps.is_empty() {
        return Err("Add at least one step.".into());
    }
    if workflow.steps[0].parallel {
        return Err("Step 1 can't run alongside a previous step.".into());
    }
    for (index, step) in workflow.steps.iter().enumerate() {
        let n = index + 1;
        if !agent_exists(&step.agent_id) {
            return Err(format!("Step {n} needs a familiar."));
        }
        if step.prompt.trim().is_empty() {
            return Err(format!("Step {n} needs instructions."));
        }
        if let Some(check) = &step.check {
            if check.phrase.trim().is_empty() {
                return Err(format!("Step {n}'s check needs a phrase to look for."));
            }
            let Some(target) = workflow.steps.get(check.go_to) else {
                return Err(format!("Step {n}'s check goes to a step that doesn't exist."));
            };
            if target.parallel {
                return Err(format!(
                    "Step {n}'s check can't go to step {}, because that step runs alongside the one before it. Pick the first step of the group.",
                    check.go_to + 1
                ));
            }
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
        steps: steps
            .into_iter()
            .map(|s| WorkflowStep {
                prompt: s.prompt.trim().to_string(),
                check: s.check.map(|c| StepCheck { phrase: c.phrase.trim().to_string(), ..c }),
                ..s
            })
            .collect(),
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

/// Sends one step's prompt to a familiar and waits for the reply. The runtime in the app, a fake in tests.
pub trait Turns: Clone + Send + Sync + 'static {
    fn turn(&self, agent_id: String, prompt: String, source: String) -> impl Future<Output = TurnOutcome> + Send + 'static;
}

impl Turns for Arc<Runtime> {
    fn turn(&self, agent_id: String, prompt: String, source: String) -> impl Future<Output = TurnOutcome> + Send + 'static {
        let runtime = self.clone();
        async move { runtime.send_and_wait(&agent_id, prompt, Some(&source)).await }
    }
}

pub type Emit = Arc<dyn Fn(WorkflowProgress) + Send + Sync>;

/// One cast in progress.
pub struct Cast {
    pub workflow: Workflow,
    pub run_id: String,
    pub input: String,
    /// Familiar names by id, for labeling replies from steps that ran together.
    pub names: HashMap<String, String>,
    pub stop: Arc<AtomicBool>,
}

/// Runs a cast to the end, recording every step, and returns its final state and error.
pub async fn execute<T: Turns>(db: Arc<Db>, turns: T, cast: Cast, emit: Emit) -> (&'static str, Option<String>) {
    let (state, error) = run_groups(&db, &turns, &cast, &emit).await;
    let _ = workflow_runs::finish(&db, &cast.run_id, state, error.as_deref());
    emit(progress(&cast, &[], state, error.clone()));
    (state, error)
}

fn progress(cast: &Cast, range: &[usize], state: &'static str, error: Option<String>) -> WorkflowProgress {
    let steps: Vec<usize> = range.iter().map(|i| i + 1).collect();
    WorkflowProgress {
        workflow_id: cast.workflow.id.clone(),
        run_id: cast.run_id.clone(),
        step: steps.first().copied().unwrap_or(cast.workflow.steps.len()),
        total: cast.workflow.steps.len(),
        agent_id: range.first().map(|&i| cast.workflow.steps[i].agent_id.clone()),
        agents: range.iter().map(|&i| cast.workflow.steps[i].agent_id.clone()).collect(),
        steps,
        state,
        error,
    }
}

async fn run_groups<T: Turns>(db: &Arc<Db>, turns: &T, cast: &Cast, emit: &Emit) -> (&'static str, Option<String>) {
    let steps = &cast.workflow.steps;
    let groups = groups(steps);
    let group_of = |step: usize| groups.iter().position(|g| g.contains(&step)).unwrap_or(groups.len());
    let source = format!("{}{}", crate::runtime::SPELL_SOURCE, cast.workflow.name);
    let mut loops: HashMap<usize, u32> = HashMap::new();
    let mut previous: Option<(String, bool)> = None;
    let mut current = 0;
    while let Some(range) = groups.get(current).cloned() {
        if cast.stop.load(Ordering::SeqCst) {
            return ("stopped", None);
        }
        let indices: Vec<usize> = range.collect();
        emit(progress(cast, &indices, "running", None));
        let mut handles = Vec::new();
        for &index in &indices {
            let step = &steps[index];
            let prompt = step_prompt(&step.prompt, &cast.input, previous.as_ref().map(|p| p.0.as_str()), previous.as_ref().is_some_and(|p| p.1));
            let row = workflow_runs::start_step(db, &cast.run_id, index, &step.agent_id, &prompt).unwrap_or_default();
            let (db, turns, emit) = (db.clone(), turns.clone(), emit.clone());
            let (agent_id, source) = (step.agent_id.clone(), source.clone());
            let running = progress(cast, &indices, "running", None);
            handles.push(tokio::spawn(async move {
                let outcome = turns.turn(agent_id, prompt, source).await;
                let (state, reply, error) = match &outcome {
                    TurnOutcome::Done(text) => ("done", Some(text.as_str()), None),
                    TurnOutcome::Stopped => ("stopped", None, None),
                    TurnOutcome::Failed(reason) => ("failed", None, Some(reason.as_str())),
                };
                let _ = workflow_runs::finish_step(&db, row, state, reply, error);
                emit(running);
                (row, outcome)
            }));
        }
        let mut outcomes = Vec::new();
        for handle in handles {
            outcomes.push(handle.await.unwrap_or_else(|e| (0, TurnOutcome::Failed(e.to_string()))));
        }
        let mut replies = Vec::new();
        let mut failure = None;
        let mut stopped = false;
        for (&index, (row, outcome)) in indices.iter().zip(&outcomes) {
            match outcome {
                TurnOutcome::Done(text) => replies.push((index, *row, text.as_str())),
                TurnOutcome::Stopped => stopped = true,
                TurnOutcome::Failed(reason) if failure.is_none() => {
                    let label = if indices.len() > 1 { format!("Step {}: ", index + 1) } else { String::new() };
                    failure = Some(format!("{label}{reason}"));
                }
                TurnOutcome::Failed(_) => {}
            }
        }
        if let Some(reason) = failure {
            return ("failed", Some(reason));
        }
        if stopped || cast.stop.load(Ordering::SeqCst) {
            return ("stopped", None);
        }
        let named: Vec<(usize, &str, &str)> = replies
            .iter()
            .map(|&(index, _, text)| (index, cast.names.get(&steps[index].agent_id).map_or("A familiar", String::as_str), text))
            .collect();
        previous = Some((join_replies(&named), replies.len() > 1));

        current += 1;
        for &(index, row, text) in &replies {
            let Some(check) = steps[index].check.as_ref().filter(|c| c.matches(text)) else { continue };
            let count = loops.entry(index).or_insert(0);
            if *count >= MAX_LOOPS {
                return (
                    "failed",
                    Some(format!(
                        "Step {}'s check matched again after sending the spell to step {} {MAX_LOOPS} times, so the spell stopped.",
                        index + 1,
                        check.go_to + 1
                    )),
                );
            }
            *count += 1;
            let _ = workflow_runs::set_went_to(db, row, check.go_to);
            current = group_of(check.go_to);
            break;
        }
    }
    ("done", None)
}

struct Live {
    run_id: String,
    agents: Vec<String>,
    stop: Arc<AtomicBool>,
}

pub struct WorkflowRunner {
    db: Arc<Db>,
    runtime: Arc<Runtime>,
    app: AppHandle,
    running: Mutex<HashMap<String, Live>>,
}

impl WorkflowRunner {
    pub fn new(db: Arc<Db>, runtime: Arc<Runtime>, app: AppHandle) -> Arc<Self> {
        if let Err(e) = workflow_runs::close_unfinished(&db) {
            eprintln!("Couldn't close unfinished spell casts: {e}");
        }
        Arc::new(WorkflowRunner { db, runtime, app, running: Mutex::new(HashMap::new()) })
    }

    pub fn running(&self) -> Vec<String> {
        self.running.lock().map(|r| r.keys().cloned().collect()).unwrap_or_default()
    }

    /// Starts a cast and returns its run id. `ritual` names the ritual that cast it, if one did.
    pub fn start(self: &Arc<Self>, workflow_id: &str, input: String, ritual: Option<&str>) -> Result<String, String> {
        let workflow = self
            .db
            .list_workflows()?
            .into_iter()
            .find(|w| w.id == workflow_id)
            .ok_or("That spell no longer exists.")?;
        let stop = Arc::new(AtomicBool::new(false));
        let run_id = {
            let mut running = self.running.lock().map_err(|_| "workflow state poisoned")?;
            if running.contains_key(workflow_id) {
                return Err("This spell is already being cast.".into());
            }
            let run_id = workflow_runs::start(&self.db, workflow_id, &input, ritual)?;
            running.insert(workflow_id.to_string(), Live { run_id: run_id.clone(), agents: Vec::new(), stop: stop.clone() });
            run_id
        };
        let names = self.db.list_agents()?.into_iter().map(|a| (a.id, a.name)).collect();
        let cast = Cast { workflow, run_id: run_id.clone(), input, names, stop };
        let runner = self.clone();
        let emit: Emit = Arc::new(move |progress| runner.progress(progress));
        let (db, runtime) = (self.db.clone(), self.runtime.clone());
        tauri::async_runtime::spawn(async move { execute(db, runtime, cast, emit).await });
        Ok(run_id)
    }

    pub async fn stop(&self, workflow_id: &str) -> Result<(), String> {
        let agents = match self.running.lock().ok().as_ref().and_then(|r| r.get(workflow_id)) {
            Some(live) => {
                live.stop.store(true, Ordering::SeqCst);
                live.agents.clone()
            }
            None => return Ok(()),
        };
        for agent_id in agents {
            self.runtime.stop(&agent_id).await?;
        }
        Ok(())
    }

    fn progress(&self, progress: WorkflowProgress) {
        if let Ok(mut running) = self.running.lock() {
            if progress.state == "running" {
                if let Some(live) = running.get_mut(&progress.workflow_id).filter(|l| l.run_id == progress.run_id) {
                    live.agents = progress.agents.clone();
                }
            } else {
                running.remove(&progress.workflow_id);
            }
        }
        let _ = self.app.emit("workflow_progress", progress);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{now_ms, Access, Agent, Avatar, Provider, WorkflowStep};
    use std::time::Duration;

    fn step(agent: &str, prompt: &str) -> WorkflowStep {
        WorkflowStep { agent_id: agent.into(), prompt: prompt.into(), ..Default::default() }
    }

    fn alongside(agent: &str, prompt: &str) -> WorkflowStep {
        WorkflowStep { parallel: true, ..step(agent, prompt) }
    }

    fn check(when: CheckWhen, phrase: &str, go_to: usize) -> Option<StepCheck> {
        Some(StepCheck { when, phrase: phrase.into(), go_to })
    }

    fn workflow(steps: Vec<WorkflowStep>) -> Workflow {
        Workflow { id: "w".into(), name: "Ship".into(), steps, created_at: 0, created_by: None }
    }

    #[test]
    fn fills_placeholders_and_appends_previous_output() {
        assert_eq!(step_prompt("Research {{input}}", "solar", None, false), "Research solar");
        assert_eq!(step_prompt("Summarize: {{previous}}", "solar", Some("notes"), false), "Summarize: notes");
        assert_eq!(
            step_prompt("Write a tweet", "solar", Some("notes"), false),
            "Write a tweet\n\nResult from the previous step:\nnotes"
        );
        assert_eq!(step_prompt("First: {{previous}}", "the input", None, false), "First: the input");
        assert_eq!(step_prompt("Merge", "x", Some("a\n\nb"), true), "Merge\n\nResults from the previous steps:\na\n\nb");
    }

    #[test]
    fn validates_steps() {
        let mut workflow = workflow(vec![step("a", "go")]);
        assert!(validate(&workflow, |id| id == "a").is_ok());
        assert!(validate(&workflow, |_| false).is_err());
        workflow.steps[0].prompt = " ".into();
        assert!(validate(&workflow, |_| true).is_err());
        workflow.steps.clear();
        assert!(validate(&workflow, |_| true).is_err());
    }

    #[test]
    fn validates_parallel_steps_and_checks() {
        let ok = |steps| validate(&workflow(steps), |_| true);
        assert_eq!(ok(vec![alongside("a", "go")]).unwrap_err(), "Step 1 can't run alongside a previous step.");
        assert!(ok(vec![step("a", "go"), alongside("b", "go")]).is_ok());

        let checked = |c: Option<StepCheck>| vec![step("a", "write"), alongside("c", "art"), WorkflowStep { check: c, ..step("b", "review") }];
        assert!(ok(checked(check(CheckWhen::Lacks, "APPROVED", 0))).is_ok());
        assert!(ok(checked(check(CheckWhen::Lacks, "APPROVED", 2))).is_ok(), "a check may retry its own step");
        assert!(ok(checked(check(CheckWhen::Lacks, "APPROVED", 3))).unwrap_err().contains("doesn't exist"));
        assert!(ok(checked(check(CheckWhen::Lacks, " ", 0))).unwrap_err().contains("needs a phrase"));
        assert!(ok(checked(check(CheckWhen::Lacks, "APPROVED", 1))).unwrap_err().contains("can't go to step 2"));
    }

    #[test]
    fn groups_steps_that_run_alongside() {
        let steps = vec![step("a", "1"), step("b", "2"), alongside("c", "3"), alongside("a", "4"), step("d", "5")];
        assert_eq!(groups(&steps), vec![0..1, 1..4, 4..5]);
        assert_eq!(join_replies(&[(0, "Ada", "only")]), "only");
        assert_eq!(
            join_replies(&[(1, "Ada", "one "), (2, "Bo", "two"), (3, "Ada", "three")]),
            "Ada (step 2) replied:\none\n\nBo replied:\ntwo\n\nAda (step 4) replied:\nthree"
        );
    }

    #[test]
    fn checks_match_ignoring_case() {
        let lacks = StepCheck { when: CheckWhen::Lacks, phrase: "approved".into(), go_to: 0 };
        assert!(lacks.matches("Needs work"));
        assert!(!lacks.matches("APPROVED, ship it"));
        let contains = StepCheck { when: CheckWhen::Contains, ..lacks };
        assert!(contains.matches("Approved"));
    }

    #[test]
    fn old_steps_still_load() {
        let steps: Vec<WorkflowStep> = serde_json::from_str(r#"[{"agentId":"a","prompt":"Research {{input}}"}]"#).unwrap();
        assert_eq!(steps, vec![step("a", "Research {{input}}")]);
        assert_eq!(groups(&steps), vec![0..1]);
    }

    /// Answers each prompt from a script keyed by familiar, and records what each familiar was sent.
    #[derive(Clone, Default)]
    struct Fake {
        sent: Arc<Mutex<Vec<(String, String)>>>,
        replies: Arc<Mutex<HashMap<String, Vec<TurnOutcome>>>>,
        delay: HashMap<String, u64>,
    }

    impl Fake {
        fn replies(self, agent: &str, outcomes: Vec<TurnOutcome>) -> Self {
            self.replies.lock().unwrap().insert(agent.into(), outcomes);
            self
        }
    }

    impl Turns for Fake {
        fn turn(&self, agent_id: String, prompt: String, _source: String) -> impl Future<Output = TurnOutcome> + Send + 'static {
            let fake = self.clone();
            async move {
                tokio::time::sleep(Duration::from_millis(*fake.delay.get(&agent_id).unwrap_or(&0))).await;
                fake.sent.lock().unwrap().push((agent_id.clone(), prompt));
                let mut replies = fake.replies.lock().unwrap();
                let queue = replies.entry(agent_id.clone()).or_default();
                if queue.is_empty() {
                    TurnOutcome::Done(format!("{agent_id} done"))
                } else {
                    queue.remove(0)
                }
            }
        }
    }

    fn agent(id: &str, name: &str) -> Agent {
        Agent {
            id: id.into(),
            name: name.into(),
            provider: Provider::Claude,
            model: None,
            workspace: "/tmp".into(),
            access: Access::Workspace,
            session_id: None,
            avatar: Avatar::for_seed(id),
            effort: None,
            active_model: None,
            coven_id: None,
            created_at: now_ms(),
        }
    }

    struct Setup {
        db: Arc<Db>,
        events: Arc<Mutex<Vec<WorkflowProgress>>>,
    }

    fn setup(steps: Vec<WorkflowStep>) -> (Setup, Cast, Emit) {
        let db = Arc::new(Db::open_in_memory().unwrap());
        for (id, name) in [("w", "Writer"), ("e", "Editor"), ("r", "Researcher"), ("a", "Artist")] {
            db.insert_agent(&agent(id, name)).unwrap();
        }
        let workflow = save(&db, None, "Post", steps, None).unwrap();
        let run_id = workflow_runs::start(&db, &workflow.id, "otters", None).unwrap();
        let names = db.list_agents().unwrap().into_iter().map(|a| (a.id, a.name)).collect();
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = events.clone();
        let emit: Emit = Arc::new(move |p| sink.lock().unwrap().push(p));
        let cast = Cast { workflow, run_id, input: "otters".into(), names, stop: Arc::new(AtomicBool::new(false)) };
        (Setup { db, events }, cast, emit)
    }

    fn done(text: &str) -> TurnOutcome {
        TurnOutcome::Done(text.into())
    }

    #[tokio::test]
    async fn records_every_step_of_a_cast() {
        let (s, cast, emit) = setup(vec![step("r", "Research {{input}}"), step("w", "Write it up")]);
        let run_id = cast.run_id.clone();
        let fake = Fake::default().replies("r", vec![done("Otters hold hands.")]);
        assert_eq!(execute(s.db.clone(), fake, cast, emit).await, ("done", None));

        let run = workflow_runs::detail(&s.db, &run_id).unwrap().unwrap();
        assert_eq!(run.run.state, "done");
        assert_eq!(run.run.input, "otters");
        assert!(run.run.ended_at.is_some());
        let sent: Vec<(usize, &str, &str, Option<&str>)> =
            run.steps.iter().map(|s| (s.step, s.agent_id.as_str(), s.prompt.as_str(), s.reply.as_deref())).collect();
        assert_eq!(
            sent,
            vec![
                (0, "r", "Research otters", Some("Otters hold hands.")),
                (1, "w", "Write it up\n\nResult from the previous step:\nOtters hold hands.", Some("w done")),
            ]
        );
        assert!(run.steps.iter().all(|s| s.state == "done" && s.ended_at.is_some()));
        let events = s.events.lock().unwrap();
        assert_eq!(events.first().unwrap().steps, vec![1]);
        assert_eq!(events.last().unwrap().state, "done");
        assert!(events.iter().all(|e| e.run_id == run_id));
    }

    #[tokio::test]
    async fn parallel_steps_start_together_and_the_next_step_gets_every_reply() {
        let (s, cast, emit) = setup(vec![
            step("r", "Research {{input}}"),
            step("w", "Write about {{previous}}"),
            alongside("a", "Draw it"),
            step("e", "Combine"),
        ]);
        let run_id = cast.run_id.clone();
        let mut fake = Fake::default().replies("w", vec![done("A post.")]).replies("a", vec![done("A sketch.")]);
        // The artist starts first but finishes last, so the join waits for both.
        fake.delay.insert("w".into(), 5);
        fake.delay.insert("a".into(), 30);
        assert_eq!(execute(s.db.clone(), fake.clone(), cast, emit).await.0, "done");

        let sent = fake.sent.lock().unwrap().clone();
        assert_eq!(sent[1], ("w".into(), "Write about r done".into()));
        assert_eq!(sent[2], ("a".into(), "Draw it\n\nResult from the previous step:\nr done".into()));
        assert_eq!(
            sent[3],
            ("e".into(), "Combine\n\nResults from the previous steps:\nWriter replied:\nA post.\n\nArtist replied:\nA sketch.".into())
        );
        let events = s.events.lock().unwrap();
        assert!(events.iter().any(|e| e.steps == vec![2, 3]));
        let run = workflow_runs::detail(&s.db, &run_id).unwrap().unwrap();
        assert_eq!(run.steps.iter().map(|s| s.step).collect::<Vec<_>>(), vec![0, 1, 2, 3]);
    }

    #[tokio::test]
    async fn a_check_sends_the_spell_back_until_the_reply_passes() {
        let review = WorkflowStep { check: check(CheckWhen::Lacks, "approved", 0), ..step("e", "Review") };
        let (s, cast, emit) = setup(vec![step("w", "Write about {{input}}"), review, step("r", "Publish")]);
        let run_id = cast.run_id.clone();
        let fake = Fake::default()
            .replies("w", vec![done("Draft 1"), done("Draft 2")])
            .replies("e", vec![done("Too long."), done("Approved.")]);
        assert_eq!(execute(s.db.clone(), fake.clone(), cast, emit).await, ("done", None));

        let sent = fake.sent.lock().unwrap().clone();
        let order: Vec<&str> = sent.iter().map(|(a, _)| a.as_str()).collect();
        assert_eq!(order, vec!["w", "e", "w", "e", "r"]);
        // The writer's second turn gets the editor's notes as its previous reply.
        assert_eq!(sent[2].1, "Write about otters\n\nResult from the previous step:\nToo long.");
        let run = workflow_runs::detail(&s.db, &run_id).unwrap().unwrap();
        assert_eq!(run.steps.iter().map(|s| s.went_to).collect::<Vec<_>>(), vec![None, Some(0), None, None, None]);
    }

    #[tokio::test]
    async fn a_check_loops_at_most_three_times() {
        let review = WorkflowStep { check: check(CheckWhen::Contains, "redo", 0), ..step("e", "Review") };
        let (s, cast, emit) = setup(vec![step("w", "Write"), review]);
        let run_id = cast.run_id.clone();
        let fake = Fake::default().replies("e", vec![done("Redo"); 10]);
        let (state, error) = execute(s.db.clone(), fake.clone(), cast, emit).await;
        assert_eq!(state, "failed");
        assert_eq!(
            error.as_deref(),
            Some("Step 2's check matched again after sending the spell to step 1 3 times, so the spell stopped.")
        );
        assert_eq!(fake.sent.lock().unwrap().len(), 8, "the first pass plus three loops of two steps");
        let run = workflow_runs::detail(&s.db, &run_id).unwrap().unwrap();
        assert_eq!(run.run.state, "failed");
        assert_eq!(run.run.error, error);
    }

    #[tokio::test]
    async fn a_failed_or_stopped_step_ends_the_cast() {
        let (s, cast, emit) = setup(vec![step("r", "Research"), alongside("a", "Draw"), step("w", "Write")]);
        let run_id = cast.run_id.clone();
        let fake = Fake::default().replies("a", vec![TurnOutcome::Failed("Out of credits.".into())]);
        assert_eq!(execute(s.db.clone(), fake.clone(), cast, emit).await, ("failed", Some("Step 2: Out of credits.".into())));
        assert_eq!(fake.sent.lock().unwrap().len(), 2);
        let run = workflow_runs::detail(&s.db, &run_id).unwrap().unwrap();
        assert_eq!(run.steps[1].state, "failed");
        assert_eq!(run.steps[1].error.as_deref(), Some("Out of credits."));

        let (s, cast, emit) = setup(vec![step("r", "Research"), step("w", "Write")]);
        cast.stop.store(true, Ordering::SeqCst);
        assert_eq!(execute(s.db.clone(), Fake::default(), cast, emit).await, ("stopped", None));
    }
}
