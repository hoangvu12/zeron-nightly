//! The steps every sandbox goes through, whatever its provider
//! (docs/design/cloud-byo.md "Steps"), and how far a chat's sandbox got: what
//! the chat's progress line shows (the step it's on, since when) and what
//! `SandboxSteps` answers.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Step {
    Create,
    Start,
    InstallZeron,
    SignIn,
    Secrets,
    GetCode,
    Setup,
    Ready,
    /// Zeron's engine winds down, then the provider stops it (#43).
    Sleep,
    /// The provider starts it, then a fresh token (#43).
    Wake,
    /// Zeron stops lending, then the provider deletes it (#44).
    Delete,
}

impl Step {
    /// A new chat's sandbox, from nothing to running its first message.
    pub const STARTING: [Step; 8] = [
        Step::Create,
        Step::Start,
        Step::InstallZeron,
        Step::SignIn,
        Step::Secrets,
        Step::GetCode,
        Step::Setup,
        Step::Ready,
    ];

    /// Every step, as step scripts name them.
    pub const ALL: [Step; 11] = [
        Step::Create,
        Step::Start,
        Step::InstallZeron,
        Step::SignIn,
        Step::Secrets,
        Step::GetCode,
        Step::Setup,
        Step::Ready,
        Step::Sleep,
        Step::Wake,
        Step::Delete,
    ];
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StepState {
    Pending,
    Running,
    Done,
    /// Its provider can't do it, or there was nothing to do.
    Skipped,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SandboxState {
    /// Its steps are running.
    Starting,
    /// Every step is done: the chat runs there.
    Ready,
    /// A step failed: the chat says why and offers Run again.
    Failed,
    /// The sandbox was deleted outside Zeron: the chat says "Sandbox
    /// deleted" and offers Start fresh.
    Gone,
    /// Going to sleep: Zeron's engine winds down, then the provider stops
    /// it (`step` is [`Step::Sleep`]).
    Sleeping,
    /// Stopped, its disk kept; the host device stands in for it. A send
    /// wakes it; the chat offers Wake.
    Asleep,
    /// Waking: the provider starts it, then its engine with a fresh token
    /// (`step` is [`Step::Wake`]). A failed wake reads [`Self::Failed`] at
    /// that step, and Run again wakes it again.
    Waking,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ScriptState {
    Running,
    /// It failed; the chat says so and offers Run again. It holds nothing
    /// up: the steps went on.
    Failed,
}

/// A step script that is running, or failed: what the chat's line above
/// the composer shows ("Running setup.sh", "Setup failed
/// reportly/.zeron/setup.sh", Run again).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScriptRun {
    /// Its moment's folder (`after-setup`), or `setup` for a repo's
    /// `.zeron/setup.sh`.
    pub hook: String,
    /// The step that moment belongs to.
    pub step: Step,
    /// Its file name.
    pub name: String,
    /// Where it is, as the chat names it (`reportly/.zeron/setup.sh`).
    pub script: String,
    #[serde(rename = "for")]
    pub scope: super::scripts::Scope,
    pub state: ScriptState,
    /// When it began, or failed, epoch ms.
    pub since: i64,
    /// What it said when it failed.
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StepProgress {
    pub step: Step,
    pub state: StepState,
}

/// How far a chat's sandbox got.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxSteps {
    pub chat_id: String,
    /// The provider's id.
    pub provider: String,
    #[serde(default)]
    pub template: Option<String>,
    /// The provider's id for it, once it has one.
    #[serde(default)]
    pub sandbox_id: Option<String>,
    /// The Zeron device running there: the chat's host.
    pub device_id: String,
    pub state: SandboxState,
    /// The step it's on, or the one that failed.
    #[serde(default)]
    pub step: Option<Step>,
    /// When `step` began (or it got ready, or failed), epoch ms: the
    /// progress line's timer.
    pub since: i64,
    /// What went wrong, in the words of whatever failed.
    #[serde(default)]
    pub error: Option<String>,
    /// What failed failed because the sandbox's disk is full: the chat
    /// says so, with Start fresh beside Run again.
    #[serde(default)]
    pub disk_full: bool,
    pub steps: Vec<StepProgress>,
    /// The step script running now, and those that failed.
    #[serde(default)]
    pub scripts: Vec<ScriptRun>,
    /// Its last start (a new chat's, or Start fresh's), and when it got
    /// ready: the chat's "Sandbox ready 52s" ("Fresh sandbox ready 58s").
    /// Kept through sleep and wake; none for a record from before it.
    #[serde(default)]
    pub started: Option<Started>,
}

/// A sandbox's start, from its first step to ready.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Started {
    /// When it began, epoch ms.
    pub at: i64,
    /// When it got ready, epoch ms (none while it starts).
    #[serde(default)]
    pub ready_at: Option<i64>,
    /// It was Start fresh: a new sandbox for a chat that had one.
    #[serde(default)]
    pub fresh: bool,
}

impl SandboxSteps {
    pub(crate) fn new(
        chat_id: &str,
        provider: &str,
        template: Option<String>,
        device_id: &str,
    ) -> Self {
        let now = crate::now_ms();
        Self {
            chat_id: chat_id.to_owned(),
            provider: provider.to_owned(),
            template,
            sandbox_id: None,
            device_id: device_id.to_owned(),
            state: SandboxState::Starting,
            step: Some(Step::STARTING[0]),
            since: now,
            error: None,
            disk_full: false,
            steps: Step::STARTING
                .iter()
                .map(|&step| StepProgress {
                    step,
                    state: StepState::Pending,
                })
                .collect(),
            scripts: Vec::new(),
            started: Some(Started {
                at: now,
                ready_at: None,
                fresh: false,
            }),
        }
    }

    pub fn state_of(&self, step: Step) -> StepState {
        self.steps
            .iter()
            .find(|s| s.step == step)
            .map_or(StepState::Pending, |s| s.state)
    }

    fn set(&mut self, step: Step, state: StepState) {
        if let Some(progress) = self.steps.iter_mut().find(|s| s.step == step) {
            progress.state = state;
        }
    }

    pub(crate) fn begin(&mut self, step: Step) {
        self.set(step, StepState::Running);
        self.state = SandboxState::Starting;
        self.step = Some(step);
        self.since = crate::now_ms();
        self.error = None;
        self.disk_full = false;
    }

    pub(crate) fn finish(&mut self, step: Step, state: StepState) {
        self.set(step, state);
    }

    pub(crate) fn fail(&mut self, step: Step, error: String) {
        self.set(step, StepState::Failed);
        self.state = SandboxState::Failed;
        self.step = Some(step);
        self.since = crate::now_ms();
        self.disk_full = disk_full(&error);
        self.error = Some(if self.disk_full {
            format!("The sandbox's disk is full. Free some space, or start fresh.\n{error}")
        } else {
            error
        });
    }

    /// Its sandbox was deleted outside Zeron.
    pub(crate) fn gone(&mut self) {
        self.state = SandboxState::Gone;
        self.since = crate::now_ms();
        self.disk_full = false;
        self.error = Some("The sandbox was deleted outside Zeron.".into());
    }

    /// Start fresh: every step again, on a new sandbox (`sandbox`: the
    /// shared place's, which Zeron doesn't make).
    pub(crate) fn fresh(&mut self, sandbox: Option<String>) {
        let mut fresh = Self::new(
            &self.chat_id,
            &self.provider,
            self.template.take(),
            &self.device_id,
        );
        fresh.sandbox_id = sandbox;
        if let Some(started) = &mut fresh.started {
            started.fresh = true;
        }
        *self = fresh;
    }

    /// Going to sleep.
    pub(crate) fn sleeping(&mut self) {
        self.state = SandboxState::Sleeping;
        self.step = Some(Step::Sleep);
        self.since = crate::now_ms();
        self.error = None;
    }

    /// Asleep: stopped, its disk kept.
    pub(crate) fn asleep(&mut self) {
        self.state = SandboxState::Asleep;
        self.step = None;
        self.since = crate::now_ms();
        self.error = None;
        self.disk_full = false;
    }

    /// Waking up.
    pub(crate) fn waking(&mut self) {
        self.state = SandboxState::Waking;
        self.step = Some(Step::Wake);
        self.since = crate::now_ms();
        self.error = None;
        self.disk_full = false;
    }

    /// It's stopped (asleep, or a wake that failed): the host device holds
    /// its room and a send wakes it.
    pub fn stopped(&self) -> bool {
        self.state == SandboxState::Asleep
            || (self.state == SandboxState::Failed && self.step == Some(Step::Wake))
    }

    pub(crate) fn ready(&mut self) {
        self.state = SandboxState::Ready;
        self.step = None;
        self.since = crate::now_ms();
        self.error = None;
        // (Its first time ready since it started: a wake isn't a start.)
        if let Some(started) = &mut self.started
            && started.ready_at.is_none()
        {
            started.ready_at = Some(self.since);
        }
    }

    /// Run again: the failed step (and what follows) runs once more.
    pub(crate) fn retry(&mut self) {
        for progress in &mut self.steps {
            if progress.state == StepState::Failed {
                progress.state = StepState::Pending;
            }
        }
        self.state = SandboxState::Starting;
        self.since = crate::now_ms();
        self.error = None;
        self.disk_full = false;
    }

    /// After a restart: a step that was running when Zeron stopped failed.
    pub(crate) fn interrupted(&mut self) {
        for script in &mut self.scripts {
            if script.state == ScriptState::Running {
                script.state = ScriptState::Failed;
                script.error = Some("Zeron restarted while this script ran.".into());
            }
        }
        match self.state {
            // Whether it got to stop is the provider's to say: the host
            // device asks it ([`super::Sandboxes::keep`]).
            SandboxState::Sleeping => self.ready(),
            SandboxState::Waking => {
                self.fail(Step::Wake, "Zeron restarted while this step ran.".into());
            }
            _ => {}
        }
        if self.state == SandboxState::Starting {
            let step = self
                .steps
                .iter()
                .find(|s| matches!(s.state, StepState::Running | StepState::Pending))
                .map(|s| s.step);
            if let Some(step) = step {
                self.fail(step, "Zeron restarted while this step ran.".into());
            }
        }
    }
}

/// Whether a command's words say the disk is full (Linux, git, npm, pip).
pub(crate) fn disk_full(said: &str) -> bool {
    let said = said.to_ascii_lowercase();
    ["no space left on device", "enospc", "disk quota exceeded"]
        .iter()
        .any(|words| said.contains(words))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_disk_is_said_as_such() {
        let mut steps = SandboxSteps::new("chat", "fake", None, "device");
        steps.begin(Step::GetCode);
        steps.fail(
            Step::GetCode,
            "Couldn't clone x:\nfatal: write error: No space left on device".into(),
        );
        assert!(steps.disk_full);
        assert!(
            steps
                .error
                .as_deref()
                .unwrap()
                .starts_with("The sandbox's disk is full.")
        );
        steps.retry();
        assert!(!steps.disk_full);
        steps.fail(Step::GetCode, "fatal: repository not found".into());
        assert!(!steps.disk_full);
        assert!(disk_full("npm ERR! code ENOSPC"));
    }

    #[test]
    fn starting_fresh_runs_every_step_again_on_the_same_device() {
        let mut steps = SandboxSteps::new("chat", "fake", Some("tpl".into()), "device");
        steps.sandbox_id = Some("old".into());
        steps.ready();
        steps.gone();
        assert_eq!(steps.state, SandboxState::Gone);
        assert_eq!(serde_json::to_value(&steps).unwrap()["state"], "gone");
        steps.fresh(None);
        assert_eq!(steps.state, SandboxState::Starting);
        assert_eq!(steps.sandbox_id, None);
        assert_eq!(steps.device_id, "device");
        assert_eq!(steps.template.as_deref(), Some("tpl"));
        assert!(steps.steps.iter().all(|s| s.state == StepState::Pending));
    }

    /// "Sandbox ready 52s": when its start began and when it got ready, a
    /// wake leaving that be; Start fresh is a start of its own, marked so.
    #[test]
    fn a_start_says_when_it_got_ready_and_a_wake_leaves_it_be() {
        let mut steps = SandboxSteps::new("chat", "fake", None, "device");
        let began = steps.started.clone().unwrap();
        assert_eq!(began.ready_at, None);
        assert!(!began.fresh);
        steps.begin(Step::Create);
        steps.ready();
        let ready = steps.started.clone().unwrap();
        assert_eq!(ready.at, began.at);
        assert_eq!(ready.ready_at, Some(steps.since));
        let json = serde_json::to_value(&steps).unwrap();
        assert_eq!(json["started"]["readyAt"], steps.since);
        assert_eq!(json["started"]["fresh"], false);
        steps.asleep();
        steps.waking();
        steps.ready();
        assert_eq!(steps.started, Some(ready), "a wake isn't a start");
        steps.gone();
        steps.fresh(None);
        let fresh = steps.started.clone().unwrap();
        assert!(fresh.fresh);
        assert_eq!(fresh.ready_at, None);
        steps.ready();
        assert!(steps.started.unwrap().ready_at.is_some());
        // A record from before it reads none.
        let mut old = serde_json::to_value(SandboxSteps::new("c", "fake", None, "d")).unwrap();
        old.as_object_mut().unwrap().remove("started");
        let old: SandboxSteps = serde_json::from_value(old).unwrap();
        assert_eq!(old.started, None);
    }

    #[test]
    fn a_failed_step_runs_again_and_what_was_done_stays_done() {
        let mut steps = SandboxSteps::new("chat", "fake", None, "device");
        steps.begin(Step::Create);
        steps.finish(Step::Create, StepState::Done);
        steps.begin(Step::Start);
        steps.fail(Step::Start, "no".into());
        assert_eq!(steps.state, SandboxState::Failed);
        assert_eq!(steps.step, Some(Step::Start));
        steps.retry();
        assert_eq!(steps.state, SandboxState::Starting);
        assert_eq!(steps.state_of(Step::Create), StepState::Done);
        assert_eq!(steps.state_of(Step::Start), StepState::Pending);
        assert_eq!(steps.error, None);
    }

    #[test]
    fn a_step_cut_off_by_a_restart_fails_and_can_run_again() {
        let mut steps = SandboxSteps::new("chat", "fake", None, "device");
        steps.begin(Step::Create);
        steps.interrupted();
        assert_eq!(steps.state, SandboxState::Failed);
        assert_eq!(steps.step, Some(Step::Create));
        let mut ready = SandboxSteps::new("chat", "fake", None, "device");
        ready.ready();
        ready.interrupted();
        assert_eq!(ready.state, SandboxState::Ready);
    }

    #[test]
    fn a_sleep_or_wake_cut_off_by_a_restart_is_asked_again_or_run_again() {
        let mut steps = SandboxSteps::new("chat", "fake", None, "device");
        steps.ready();
        steps.sleeping();
        assert_eq!(serde_json::to_value(&steps).unwrap()["step"], "sleep");
        steps.interrupted();
        assert_eq!(steps.state, SandboxState::Ready);
        steps.asleep();
        assert!(steps.stopped());
        assert_eq!(serde_json::to_value(&steps).unwrap()["state"], "asleep");
        steps.interrupted();
        assert_eq!(steps.state, SandboxState::Asleep);
        steps.waking();
        steps.interrupted();
        assert_eq!(steps.state, SandboxState::Failed);
        assert_eq!(steps.step, Some(Step::Wake));
        assert!(steps.stopped(), "a failed wake still holds its room");
    }

    #[test]
    fn steps_read_as_the_progress_line_names_them() {
        let steps = SandboxSteps::new("chat", "fake", None, "device");
        let json = serde_json::to_value(&steps).unwrap();
        assert_eq!(json["step"], "create");
        assert_eq!(json["state"], "starting");
        assert_eq!(
            json["steps"][2],
            serde_json::json!({"step": "installZeron", "state": "pending"})
        );
    }
}
