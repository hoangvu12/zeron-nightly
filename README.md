# Zeron nightly

Nightly Windows builds of [Zeron](https://github.com/zeronsh/zeron) v3, the
zgx interface (`apps/zgx`): upstream's `zeron/zgx` branch plus a short list of
patches that aren't (yet) upstream. Installed nightlies update themselves from
this repo's releases.

The v2 nightly (upstream `main`, the `crates/ui` interface) is no longer
built. Its old releases stay up, but get no updates.

- `main` (this branch) holds only the tooling: `patches-v3.toml`, `patches/`,
  `rerere/`, `scripts/` and the workflow.
- `nightly-v3` is generated. Every run rebuilds it from scratch, linux-next
  style, so never base work on it.

## How a nightly is made

`.github/workflows/nightly-v3.yml` runs at 19:30 UTC (02:30 in Vietnam) and
on demand (Actions → nightly-v3 → Run workflow).

1. `scripts/assemble.py` resets the tree to upstream `zeron/zgx` and applies
   each enabled `[[patch]]` in `patches-v3.toml` in order: upstream PRs at a
   pinned commit, patch files with `git am -3`, or branches of this repo.
2. A patch that no longer applies is **dropped from that build** and listed in
   the release notes. The build goes on without it, unless the entry says
   `required = true`: then the run fails and nothing ships, since installs
   update to every release. Conflicts resolved once replay from `rerere/`.
3. Upstream's own `.github/workflows` are removed (they belong to
   zeronsh/zeron), and the version becomes the next patch release's
   prerelease: upstream `0.2.139` → `0.2.140-v3.YYYYMMDDHHMM`.
4. If upstream and the patch list are unchanged since the last release, the
   run stops there. Otherwise `scripts/package-v3-windows.ps1` builds
   `zeron-zgx` with its `beta` feature (production sign-in, Zeron's own data)
   and the engine from the same tree, and packages them with the `ui` scripts
   as a portable zip and a per-user installer (`dist/zeron-v3.iss`). A
   prerelease `v<version>` is published, and the `v3-latest` release's
   installer and `manifest.json` are replaced. The last 14 releases are kept.

## Changing what goes in

Edit `patches-v3.toml`:

```toml
[[patch]]
name = "my-fix"
pr = 1234                 # upstream PR
sha = "<full commit sha>" # the commit you reviewed; new pushes wait for you

[[patch]]
name = "local-tweak"
file = "patches/0002-local-tweak.patch"  # git format-patch --full-index output

[[patch]]
name = "bigger-work"
branch = "work/bigger"    # a branch pushed to this repo
```

Set `enabled = false` to park an entry. The release notes say when a pinned
PR has new commits, is in upstream (then delete the entry) or was closed. A
PR merged into `main` is no help yet: it stays applied at its pin until
`zeron/zgx` takes it, and the notes say so.

Make patch files with `git format-patch --full-index`: CI's clone has no
blobs to fall back on, so `git am -3` needs the full ids.

To try it locally, use a checkout dedicated to the script (it resets it):

```sh
git clone https://github.com/zeronsh/zeron.git ../zeron-v3-src
python scripts/assemble.py --src ../zeron-v3-src
pwsh scripts/package-v3-windows.ps1 -Src ../zeron-v3-src
```

### When a patch conflicts

1. Run `assemble.py` locally. Note which patch was dropped.
2. In `../zeron-v3-src`, apply that patch by hand (`git merge <sha>` or
   `git am -3 patches/...`), resolve the conflict and commit. With
   `rerere.enabled` (the script sets it) git records the resolution.
3. `python scripts/assemble.py --src ../zeron-v3-src --save-rerere`, then
   commit `rerere/`. Later runs replay it, locally and in CI.

If the conflict is bigger than a resolution can carry, rebase the work onto
upstream instead and regenerate the patch file (or point the entry at a
`branch` or a new pin).

## Installing a nightly

1. Quit Zeron. Back up `%LOCALAPPDATA%\Zeron` if you want a way back.
2. Run the `v3-latest` release's installer. It installs to
   `%LOCALAPPDATA%\Programs\Zeron v3` and uses `%LOCALAPPDATA%\Zeron` and the
   engine on port 27654, as other Zeron installs do: run one app at a time.

From then on it updates itself from the `v3-latest` release: the app
downloads that installer and runs it silently over its folder, since swapping
`zeron.exe` alone would leave `ui` and the engine behind (`patches/0005`).
The engine restarts into the new one once it is idle.

## Notes

- GitHub disables scheduled workflows in a repo with no activity for 60 days.
  Any commit here (a patch-list change) resets that.
- Builds are unsigned, so Windows SmartScreen may warn on the installer.
- Releases are prereleases and never the latest: old v2 installs follow
  `releases/latest`, and must not update into a v3 build.
