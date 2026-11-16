//! Step scripts (docs/design/cloud-byo.md "Step scripts"): the user's own
//! scripts, run before, after or instead of a sandbox's steps. Files are the
//! truth, kept on the host device (the one running the steps), so Fleet can
//! sync them and git can track them:
//!
//! ```text
//! {data}/steps/<hook>/<name>               every sandbox (~/.zeron/steps on Unix)
//! {data}/steps/<provider>/<hook>/<name>    that provider's sandboxes
//! <repo>/.zeron/<hook>/<name>              that repo's
//! <repo>/.zeron/setup.sh                   that repo's Setup
//! ```
//!
//! `<hook>` is `before-`, `after-` or `instead-of-` and the step
//! (`after-setup`, `before-install-zeron`). A moment's scripts run in name
//! order, whichever folder they're in. Where one runs follows from its
//! moment ([`Hook::runs_in_sandbox`]).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::steps::Step;

/// The folder under the data dir holding the user's scripts.
pub const STEPS_DIR: &str = "steps";
/// A repo's folder of its own scripts.
pub const REPO_DIR: &str = ".zeron";
/// A repo's Setup script, in [`REPO_DIR`].
pub const REPO_SETUP: &str = "setup.sh";
/// [`Hook`]'s name for a repo's `.zeron/setup.sh` (it is the Setup step).
pub const SETUP_HOOK: &str = "setup";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Timing {
    Before,
    After,
    InsteadOf,
}

/// A moment scripts hook onto: before, after or instead of a step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hook {
    pub timing: Timing,
    pub step: Step,
}

impl Hook {
    pub const fn new(timing: Timing, step: Step) -> Self {
        Self { timing, step }
    }

    /// Its folder's name: `after-setup`, `instead-of-install-zeron`.
    pub fn name(&self) -> String {
        let timing = match self.timing {
            Timing::Before => "before",
            Timing::After => "after",
            Timing::InsteadOf => "instead-of",
        };
        format!("{timing}-{}", slug(self.step))
    }

    pub fn parse(name: &str) -> Option<Self> {
        let (timing, step) = if let Some(step) = name.strip_prefix("before-") {
            (Timing::Before, step)
        } else if let Some(step) = name.strip_prefix("after-") {
            (Timing::After, step)
        } else {
            (Timing::InsteadOf, name.strip_prefix("instead-of-")?)
        };
        let step = Step::ALL.into_iter().find(|s| slug(*s) == step)?;
        Some(Self { timing, step })
    }

    /// The sandbox is up for this moment, so its scripts run there; the
    /// others run on the host device. Before it's made or started, while
    /// it's being stopped or deleted, there's no sandbox to run in.
    pub fn runs_in_sandbox(&self) -> bool {
        match (self.step, self.timing) {
            (Step::Create | Step::Delete, _) => false,
            (Step::Start | Step::Wake, timing) => timing == Timing::After,
            (Step::Sleep, timing) => timing == Timing::Before,
            _ => true,
        }
    }
}

/// A step's name in a hook's folder name.
pub fn slug(step: Step) -> &'static str {
    match step {
        Step::Create => "create",
        Step::Start => "start",
        Step::InstallZeron => "install-zeron",
        Step::SignIn => "sign-in",
        Step::Secrets => "secrets",
        Step::GetCode => "get-code",
        Step::Setup => "setup",
        Step::Ready => "ready",
        Step::Sleep => "sleep",
        Step::Wake => "wake",
        Step::Delete => "delete",
    }
}

/// Which sandboxes a script is for: where its folder sits.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Scope {
    /// `{data}/steps/`.
    All,
    /// `{data}/steps/<provider>/`.
    Provider { provider: String },
    /// `<repo>/.zeron/`, the project's folder on the host device.
    Project {
        #[serde(rename = "spaceId")]
        space_id: String,
    },
}

impl Scope {
    pub(crate) fn rank(&self) -> u8 {
        match self {
            Scope::All => 0,
            Scope::Provider { .. } => 1,
            Scope::Project { .. } => 2,
        }
    }
}

/// One script file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Script {
    pub scope: Scope,
    /// [`Hook::name`], or [`SETUP_HOOK`] for a repo's `setup.sh`.
    pub hook: String,
    pub name: String,
    pub path: PathBuf,
    /// How the chat names it: `steps/after-setup/fleet-sync.sh`,
    /// `reportly/.zeron/setup.sh`.
    pub shown: String,
}

impl Script {
    /// Name order within a moment; on a tie, all before provider before repo.
    pub fn order(&self) -> (String, u8) {
        (self.name.clone(), self.scope.rank())
    }
}

/// A project's folder on the host device, for its own scripts.
#[derive(Clone, Debug)]
pub struct Repo {
    pub space_id: String,
    pub dir: PathBuf,
    /// Its name in what the chat shows (`reportly`).
    pub folder: String,
}

/// The scripts for `hook` on `provider`'s sandboxes for `repo`, in the order
/// they run.
pub fn for_hook(steps_dir: &Path, provider: &str, repo: Option<&Repo>, hook: Hook) -> Vec<Script> {
    let name = hook.name();
    let mut scripts = files_in(&steps_dir.join(&name))
        .into_iter()
        .map(|(file, path)| Script {
            scope: Scope::All,
            hook: name.clone(),
            shown: format!("{STEPS_DIR}/{name}/{file}"),
            name: file,
            path,
        })
        .collect::<Vec<_>>();
    if valid_name(provider) {
        scripts.extend(
            files_in(&steps_dir.join(provider).join(&name))
                .into_iter()
                .map(|(file, path)| Script {
                    scope: Scope::Provider {
                        provider: provider.to_owned(),
                    },
                    hook: name.clone(),
                    shown: format!("{STEPS_DIR}/{provider}/{name}/{file}"),
                    name: file,
                    path,
                }),
        );
    }
    if let Some(repo) = repo {
        scripts.extend(
            files_in(&repo.dir.join(REPO_DIR).join(&name))
                .into_iter()
                .map(|(file, path)| Script {
                    scope: Scope::Project {
                        space_id: repo.space_id.clone(),
                    },
                    hook: name.clone(),
                    shown: format!("{}/{REPO_DIR}/{name}/{file}", repo.folder),
                    name: file,
                    path,
                }),
        );
    }
    scripts.sort_by_key(Script::order);
    scripts
}

/// The repo's `.zeron/setup.sh`, if it has one.
pub fn repo_setup(repo: &Repo) -> Option<Script> {
    let path = repo.dir.join(REPO_DIR).join(REPO_SETUP);
    path.is_file().then(|| Script {
        scope: Scope::Project {
            space_id: repo.space_id.clone(),
        },
        hook: SETUP_HOOK.into(),
        name: REPO_SETUP.into(),
        shown: format!("{}/{REPO_DIR}/{REPO_SETUP}", repo.folder),
        path,
    })
}

/// Every script under `steps_dir` and the repos', as Settings lists them:
/// by scope (all, providers, projects), then hook, then name.
pub fn all(steps_dir: &Path, repos: &[Repo]) -> Vec<Script> {
    let mut scripts = Vec::new();
    let mut providers = Vec::new();
    for (folder, path) in dirs_in(steps_dir) {
        if Hook::parse(&folder).is_some() {
            scripts.extend(for_folder(
                Scope::All,
                &folder,
                &path,
                &format!("{STEPS_DIR}/{folder}"),
            ));
        } else if valid_name(&folder) {
            providers.push((folder, path));
        }
    }
    for (provider, dir) in providers {
        for (folder, path) in dirs_in(&dir) {
            if Hook::parse(&folder).is_some() {
                let scope = Scope::Provider {
                    provider: provider.clone(),
                };
                let shown = format!("{STEPS_DIR}/{provider}/{folder}");
                scripts.extend(for_folder(scope, &folder, &path, &shown));
            }
        }
    }
    for repo in repos {
        let zeron = repo.dir.join(REPO_DIR);
        for (folder, path) in dirs_in(&zeron) {
            if Hook::parse(&folder).is_some() {
                let scope = Scope::Project {
                    space_id: repo.space_id.clone(),
                };
                let shown = format!("{}/{REPO_DIR}/{folder}", repo.folder);
                scripts.extend(for_folder(scope, &folder, &path, &shown));
            }
        }
        scripts.extend(repo_setup(repo));
    }
    scripts.sort_by(|a, b| {
        (a.scope.rank(), scope_key(&a.scope), &a.hook, &a.name).cmp(&(
            b.scope.rank(),
            scope_key(&b.scope),
            &b.hook,
            &b.name,
        ))
    });
    scripts
}

fn scope_key(scope: &Scope) -> &str {
    match scope {
        Scope::All => "",
        Scope::Provider { provider } => provider,
        Scope::Project { space_id } => space_id,
    }
}

fn for_folder(scope: Scope, hook: &str, dir: &Path, shown: &str) -> Vec<Script> {
    files_in(dir)
        .into_iter()
        .map(|(file, path)| Script {
            scope: scope.clone(),
            hook: hook.to_owned(),
            shown: format!("{shown}/{file}"),
            name: file,
            path,
        })
        .collect()
}

/// Where the script `scope`/`hook`/`name` lives, or why it can't: names
/// are single plain file names, and hooks the ones there are.
pub fn path_of(
    steps_dir: &Path,
    repo: Option<&Repo>,
    scope: &Scope,
    hook: &str,
    name: &str,
) -> Result<PathBuf, String> {
    if !valid_name(name) {
        return Err(format!("“{name}” isn't a script's file name."));
    }
    if hook == SETUP_HOOK {
        let repo = match scope {
            Scope::Project { .. } => repo.ok_or("That project isn't on this device.")?,
            _ => return Err("Only a project has a setup.sh of its own.".into()),
        };
        if name != REPO_SETUP {
            return Err(format!("A project's Setup script is {REPO_SETUP}."));
        }
        return Ok(repo.dir.join(REPO_DIR).join(REPO_SETUP));
    }
    if Hook::parse(hook).is_none() {
        return Err(format!("“{hook}” isn't a step script's moment."));
    }
    Ok(match scope {
        Scope::All => steps_dir.join(hook).join(name),
        Scope::Provider { provider } => {
            if !valid_name(provider) || Hook::parse(provider).is_some() {
                return Err(format!("“{provider}” isn't a provider."));
            }
            steps_dir.join(provider).join(hook).join(name)
        }
        Scope::Project { .. } => repo
            .ok_or("That project isn't on this device.")?
            .dir
            .join(REPO_DIR)
            .join(hook)
            .join(name),
    })
}

/// Save a script, making its folders: its owner's only, and runnable where
/// that means something.
pub fn write(path: &Path, content: &str) -> Result<(), String> {
    crate::runner::write_private_atomic(path, content.as_bytes()).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// A script as one host device hands it to the next.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScriptFile {
    #[serde(rename = "for")]
    pub scope: Scope,
    pub hook: String,
    pub name: String,
    pub content: String,
}

/// The scripts under `steps_dir` that `provider`'s sandboxes run: every
/// sandbox's and its own. (A project's travel with its folder.)
pub fn for_provider(steps_dir: &Path, provider: &str) -> Vec<ScriptFile> {
    all(steps_dir, &[])
        .into_iter()
        .filter(|script| match &script.scope {
            Scope::All => true,
            Scope::Provider { provider: theirs } => theirs == provider,
            Scope::Project { .. } => false,
        })
        .filter_map(|script| {
            Some(ScriptFile {
                content: std::fs::read_to_string(&script.path).ok()?,
                scope: script.scope,
                hook: script.hook,
                name: script.name,
            })
        })
        .collect()
}

/// The folder of `provider`'s own scripts.
pub fn provider_dir(steps_dir: &Path, provider: &str) -> Result<PathBuf, String> {
    if !valid_name(provider) || Hook::parse(provider).is_some() {
        return Err(format!("“{provider}” isn't a provider."));
    }
    Ok(steps_dir.join(provider))
}

/// Keep `scripts` under `steps_dir` (handed over), each where its scope
/// says; one that names a place a script can't be is left out.
pub fn take(steps_dir: &Path, scripts: &[ScriptFile]) -> Result<(), String> {
    for script in scripts {
        if matches!(script.scope, Scope::Project { .. }) {
            continue;
        }
        let Ok(path) = path_of(steps_dir, None, &script.scope, &script.hook, &script.name) else {
            tracing::warn!(hook = %script.hook, name = %script.name, "sandboxes: a handed-over script names no place for a script");
            continue;
        };
        write(&path, &script.content).map_err(|e| format!("Couldn't keep {}: {e}", script.name))?;
    }
    Ok(())
}

/// A single plain file or folder name.
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.starts_with('.')
        && !name.contains(['/', '\\', ':', '\0'])
}

/// The files in `dir` (not hidden, not editor leftovers), by name.
fn files_in(dir: &Path) -> Vec<(String, PathBuf)> {
    entries(dir, |t| t.is_file())
        .into_iter()
        .filter(|(name, _)| !name.ends_with('~'))
        .collect()
}

fn dirs_in(dir: &Path) -> Vec<(String, PathBuf)> {
    entries(dir, |t| t.is_dir())
}

fn entries(dir: &Path, keep: impl Fn(&std::fs::FileType) -> bool) -> Vec<(String, PathBuf)> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<(String, PathBuf)> = read
        .flatten()
        .filter(|e| {
            // Follow links: a Fleet-synced script may be one.
            std::fs::metadata(e.path()).is_ok_and(|m| keep(&m.file_type()))
        })
        .filter_map(|e| Some((e.file_name().into_string().ok()?, e.path())))
        .filter(|(name, _)| valid_name(name))
        .collect();
    found.sort();
    found
}

/// `sh` on this device, to run host-side scripts with. On Windows it's
/// Git's (`…\Git\usr\bin\sh.exe`, found from `git` on the PATH), with its
/// tools put first on the PATH.
pub fn sh_command() -> std::process::Command {
    #[cfg(windows)]
    {
        if let Some((sh, bin)) = git_sh() {
            let mut command = std::process::Command::new(sh);
            let path = std::env::var_os("PATH").unwrap_or_default();
            let mut paths = vec![bin];
            paths.extend(std::env::split_paths(&path));
            if let Ok(joined) = std::env::join_paths(paths) {
                command.env("PATH", joined);
            }
            return command;
        }
    }
    std::process::Command::new("sh")
}

#[cfg(windows)]
fn git_sh() -> Option<(PathBuf, PathBuf)> {
    let path = std::env::var_os("PATH")?;
    let git = std::env::split_paths(&path)
        .map(|dir| dir.join("git.exe"))
        .find(|git| git.is_file())?;
    git.ancestors().skip(1).find_map(|root| {
        let bin = root.join("usr").join("bin");
        let sh = bin.join("sh.exe");
        sh.is_file().then_some((sh, bin))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hooks_name_their_folders_and_say_where_they_run() {
        for (name, timing, step, in_sandbox) in [
            ("before-create", Timing::Before, Step::Create, false),
            ("after-create", Timing::After, Step::Create, false),
            ("instead-of-start", Timing::InsteadOf, Step::Start, false),
            ("after-start", Timing::After, Step::Start, true),
            (
                "before-install-zeron",
                Timing::Before,
                Step::InstallZeron,
                true,
            ),
            ("instead-of-secrets", Timing::InsteadOf, Step::Secrets, true),
            ("after-get-code", Timing::After, Step::GetCode, true),
            ("after-setup", Timing::After, Step::Setup, true),
            ("after-wake", Timing::After, Step::Wake, true),
            ("before-wake", Timing::Before, Step::Wake, false),
            ("before-sleep", Timing::Before, Step::Sleep, true),
            ("after-sleep", Timing::After, Step::Sleep, false),
            ("before-delete", Timing::Before, Step::Delete, false),
            ("after-delete", Timing::After, Step::Delete, false),
        ] {
            let hook = Hook::parse(name).unwrap_or_else(|| panic!("{name}"));
            assert_eq!(hook, Hook::new(timing, step), "{name}");
            assert_eq!(hook.name(), name);
            assert_eq!(hook.runs_in_sandbox(), in_sandbox, "{name}");
        }
        for not in ["after", "after-lunch", "docker", "setup", "before-"] {
            assert_eq!(Hook::parse(not), None, "{not}");
        }
    }

    #[test]
    fn a_moments_scripts_run_in_name_order_from_every_folder_that_applies() {
        let dir = tempfile::tempdir().unwrap();
        let steps = dir.path().join("steps");
        let repo = Repo {
            space_id: "s".into(),
            dir: dir.path().join("reportly"),
            folder: "reportly".into(),
        };
        for path in [
            "steps/after-setup/30-c.sh",
            "steps/after-setup/10-a.sh",
            "steps/after-setup/.hidden",
            "steps/after-setup/10-a.sh~",
            "steps/docker/after-setup/20-b.sh",
            "steps/boat/after-setup/no.sh",
            "steps/after-wake/no.sh",
            "reportly/.zeron/after-setup/15-r.sh",
            "reportly/.zeron/setup.sh",
        ] {
            let path = dir.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "true").unwrap();
        }
        let hook = Hook::new(Timing::After, Step::Setup);
        let shown: Vec<String> = for_hook(&steps, "docker", Some(&repo), hook)
            .into_iter()
            .map(|s| s.shown)
            .collect();
        assert_eq!(
            shown,
            [
                "steps/after-setup/10-a.sh",
                "reportly/.zeron/after-setup/15-r.sh",
                "steps/docker/after-setup/20-b.sh",
                "steps/after-setup/30-c.sh",
            ]
        );
        assert_eq!(
            repo_setup(&repo).map(|s| s.shown).as_deref(),
            Some("reportly/.zeron/setup.sh")
        );
        // Every one but the hidden file and the editor's leftover.
        assert_eq!(all(&steps, std::slice::from_ref(&repo)).len(), 7);
    }

    #[test]
    fn a_script_lives_only_where_a_script_may() {
        let steps = Path::new("/d/steps");
        let ok = |scope: Scope, hook: &str, name: &str| path_of(steps, None, &scope, hook, name);
        assert_eq!(
            ok(Scope::All, "after-setup", "a.sh").unwrap(),
            steps.join("after-setup").join("a.sh")
        );
        assert_eq!(
            ok(
                Scope::Provider {
                    provider: "docker".into()
                },
                "after-wake",
                "a.sh"
            )
            .unwrap(),
            steps.join("docker").join("after-wake").join("a.sh")
        );
        for (scope, hook, name) in [
            (Scope::All, "after-setup", "../a.sh"),
            (Scope::All, "after-setup", "a/b.sh"),
            (Scope::All, "after-setup", r"a\b.sh"),
            (Scope::All, "after-setup", ".."),
            (Scope::All, "after-setup", ""),
            (Scope::All, "lunch", "a.sh"),
            (Scope::All, SETUP_HOOK, REPO_SETUP),
            (
                Scope::Provider {
                    provider: "..".into(),
                },
                "after-wake",
                "a.sh",
            ),
            (
                Scope::Provider {
                    provider: "after-setup".into(),
                },
                "after-wake",
                "a.sh",
            ),
            (
                Scope::Project {
                    space_id: "s".into(),
                },
                "after-setup",
                "a.sh",
            ),
        ] {
            assert!(
                ok(scope.clone(), hook, name).is_err(),
                "{scope:?} {hook} {name}"
            );
        }
    }
}
