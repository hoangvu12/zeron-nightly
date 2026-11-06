# Zeron nightly

Nightly Windows builds of [Zeron](https://github.com/zeronsh/zeron): upstream
`main` plus a short list of patches that aren't (yet) upstream. Installed
nightlies update themselves from this repo's releases.

- `main` (this branch) holds only the tooling: `patches.toml`, `patches/`,
  `rerere/`, `scripts/assemble.py` and the workflow.
- `nightly` is generated. Every run rebuilds it from scratch, linux-next
  style, so never base work on it. It appears once the optional push token
  below is set; every release also attaches `nightly.bundle` with its exact
  commits.

## How a nightly is made

`.github/workflows/nightly.yml` runs at 19:00 UTC and on demand
(Actions → nightly → Run workflow).

1. `scripts/assemble.py` resets `nightly` to upstream `main` and applies each
   enabled `[[patch]]` in order: upstream PRs at a pinned commit, patch files
   with `git am -3`, or branches of this repo.
2. A patch that no longer applies is **dropped from that build** and listed in
   the release notes. The build goes on without it. Conflicts resolved once
   replay from `rerere/`.
3. Upstream's own `.github/workflows` are removed (they belong to
   zeronsh/zeron), and the version becomes the next patch release's
   prerelease: upstream `0.2.136` → `0.2.137-nightly.YYYYMMDDHHMM`. That sorts
   above the release it was built on and below the next one.
4. If upstream and the patch list are unchanged since the last release, the
   run stops there. Otherwise Windows builds while the updater tests run on a
   second runner, and a release `v<version>` is published with the installer,
   portable zip, updater `.exe` and `manifest.json`. The last 14 releases are
   kept.

Nightlies build without LTO across crates, which cuts most of the build time.
They may run slightly slower than upstream's releases.

## Changing what goes in

Edit `patches.toml`:

```toml
[[patch]]
name = "my-fix"
pr = 1234                 # upstream PR
sha = "<full commit sha>" # the commit you reviewed; new pushes wait for you

[[patch]]
name = "local-tweak"
file = "patches/0002-local-tweak.patch"  # git format-patch output

[[patch]]
name = "bigger-work"
branch = "work/bigger"    # a branch pushed to this repo
```

Set `enabled = false` to park an entry. The release notes say when a pinned
PR has new commits, was merged upstream (then delete the entry) or was closed.

To try it locally, use a checkout dedicated to the script (it resets it):

```sh
git clone https://github.com/zeronsh/zeron.git ../zeron-nightly-src
python scripts/assemble.py --src ../zeron-nightly-src
```

### When a patch conflicts

1. Run `assemble.py` locally. Note which patch was dropped.
2. In `../zeron-nightly-src`, apply that patch by hand (`git merge <sha>` or
   `git am -3 patches/...`), resolve the conflict and commit. With
   `rerere.enabled` (the script sets it) git records the resolution.
3. `python scripts/assemble.py --src ../zeron-nightly-src --save-rerere`, then
   commit `rerere/`. Later runs replay it, locally and in CI.

If the conflict is bigger than a resolution can carry, rebase the work onto
upstream instead and point the entry at the result (a `branch` or a new pin).

## Installing a nightly

The first install is by hand, because stable Zeron's updater only accepts
plain `X.Y.Z` versions (the `windows-prerelease-versions` patch lifts that):

1. Quit Zeron. Back up `%LOCALAPPDATA%\Zeron` if you want a way back.
2. Run the latest release's `zeron-<version>-windows-x86_64-setup.exe`.
   It installs over stable and writes `zeron-update.json` pointing here.

From then on Zeron updates from
`https://github.com/hoangvu12/zeron-nightly/releases/latest/download`.

**Back to stable:** run the official installer from zeronsh/zeron. It restores
the upstream feed. A nightly may have written data a stable build doesn't
know yet; stable catches up at its next release.

## Zeron v3 nightly

v3 is the zgx interface (`apps/zgx`), which upstream builds on its
`zeron/zgx` branch. `.github/workflows/nightly-v3.yml` runs at 19:30 UTC and
on demand. It works like the v2 nightly, with these differences:

- It assembles `zeron/zgx` plus `patches-v3.toml` (`assemble.py --config
  patches-v3.toml`). The version is `0.2.137-v3.YYYYMMDDHHMM`.
- `scripts/package-v3-windows.ps1` builds `zeron-zgx` with its `beta`
  feature (production sign-in, Zeron's own data) and the engine from the same
  tree. It packages them with the `ui` scripts as a portable zip and a
  per-user installer (`dist/zeron-v3.iss`). In CI the engine, the interface
  and the updater tests build on three runners at once (`-Build engine` and
  `-Build app`). A fourth job packages the result (`-Prebuilt`). zgx also
  builds with 16 codegen units here, not its own 1.
- Releases are prereleases and never the latest, so v2 installs never see
  them. Each keeps its own last 14.
- It installs to `%LOCALAPPDATA%\Programs\Zeron v3`, next to v2. Both share
  `%LOCALAPPDATA%\Zeron` and the engine on port 27654. v3 uses an engine that
  already runs, so run one app at a time to be safe.
- v3 installs update themselves from the `v3-latest` release. Each run
  replaces its installer and `manifest.json` there. The app downloads that
  installer and runs it silently over its folder, since swapping `zeron.exe`
  alone would leave `ui` and the engine behind (`patches/0005`). The engine
  restarts into the new one once it is idle.

To try it locally, use a separate checkout from the v2 one:

```sh
git clone https://github.com/zeronsh/zeron.git ../zeron-v3-src
python scripts/assemble.py --src ../zeron-v3-src --config patches-v3.toml
pwsh scripts/package-v3-windows.ps1 -Src ../zeron-v3-src
```

## Optional: publish the `nightly` branch

GitHub's built-in token cannot push commits that touch workflow files, and
upstream's history has plenty. To publish the branch, add a fine-grained token
scoped to this repo with **Contents: read and write** and **Workflows: read
and write** as the secret `NIGHTLY_PUSH_TOKEN`.

## Notes

- GitHub disables scheduled workflows in a repo with no activity for 60 days.
  Any commit here (a patch-list change) resets that.
- Builds are unsigned, so Windows SmartScreen may warn on the installer.
