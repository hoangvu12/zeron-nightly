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
    pub steps: Vec<StepProgress>,
    /// The step script running now, and those that failed.
    #[serde(default)]
    pub scripts: Vec<ScriptRun>,
}

impl SandboxSteps {
    pub(crate) fn new(
        chat_id: &str,
        provider: &str,
        template: Option<String>,
        device_id: &str,
    ) -> Self {
        Self {
            chat_id: chat_id.to_owned(),
            provider: provider.to_owned(),
            template,
            sandbox_id: None,
            device_id: device_id.to_owned(),
            state: SandboxState::Starting,
            step: Some(Step::STARTING[0]),
            since: crate::now_ms(),
            error: None,
            steps: Step::STARTING
                .iter()
                .map(|&step| StepProgress {
                    step,
                    state: StepState::Pending,
                })
                .collect(),
            scripts: Vec::new(),
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
    }

    pub(crate) fn finish(&mut self, step: Step, state: StepState) {
        self.set(step, state);
    }

    pub(crate) fn fail(&mut self, step: Step, error: String) {
        self.set(step, StepState::Failed);
        self.state = SandboxState::Failed;
        self.step = Some(step);
        self.since = crate::now_ms();
        self.error = Some(error);
    }

    pub(crate) fn ready(&mut self) {
        self.state = SandboxState::Ready;
        self.step = None;
        self.since = crate::now_ms();
        self.error = None;
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
    }

    /// After a restart: a step that was running when Zeron stopped failed.
    pub(crate) fn interrupted(&mut self) {
        for script in &mut self.scripts {
            if script.state == ScriptState::Running {
                script.state = ScriptState::Failed;
                script.error = Some("Zeron restarted while this script ran.".into());
            }
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

#[cfg(test)]
mod tests {
    use super::*;

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
