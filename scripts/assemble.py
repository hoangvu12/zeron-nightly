#!/usr/bin/env python3
"""Assemble Zeron nightly: upstream plus patches.toml, rebuilt from scratch.

Like linux-next, the `nightly` branch is never updated in place. Every run
resets it to upstream, applies each enabled patch in order, and stamps a
nightly version. A patch that no longer applies is dropped for that run and
reported; conflicts already resolved once replay from rerere/ (git rerere).

    python scripts/assemble.py --src ../zeron-nightly-src
    python scripts/assemble.py --src ../zeron-nightly-src --save-rerere

`--src` is a git checkout dedicated to this script: it is reset and cleaned
(ignored files such as target/ survive).
"""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tomllib
import urllib.request
from dataclasses import dataclass, field
from pathlib import Path

TOOLS = Path(__file__).resolve().parent.parent
IDENT = ["-c", "user.name=zeron-nightly", "-c", "user.email=zeron-nightly@users.noreply.github.com"]


def git(src: Path, *args: str, check: bool = True) -> subprocess.CompletedProcess[str]:
    result = subprocess.run(["git", "-C", str(src), *args], text=True, capture_output=True)
    if check and result.returncode != 0:
        sys.exit(f"git {' '.join(args)} failed:\n{result.stderr.strip()}")
    return result


def out(src: Path, *args: str) -> str:
    return git(src, *args).stdout.strip()


def github(path: str) -> dict | None:
    """Best-effort GitHub API read; the build never depends on it."""
    request = urllib.request.Request(f"https://api.github.com/{path}")
    token = os.environ.get("GITHUB_TOKEN") or os.environ.get("GH_TOKEN")
    if token:
        request.add_header("Authorization", f"Bearer {token}")
    try:
        with urllib.request.urlopen(request, timeout=15) as response:
            return json.load(response)
    except Exception:
        return None


@dataclass
class Run:
    included: list[str] = field(default_factory=list)
    dropped: list[str] = field(default_factory=list)
    notices: list[str] = field(default_factory=list)
    inputs: list[str] = field(default_factory=list)


def unresolved(src: Path) -> list[str]:
    return out(src, "diff", "--name-only", "--diff-filter=U").splitlines()


def merge(src: Path, rev: str, message: str) -> str | None:
    """Merge `rev`; None on success, else why it was dropped."""
    result = git(src, *IDENT, "merge", "--no-ff", "--no-edit", "-m", message, rev, check=False)
    if result.returncode == 0:
        return None
    conflicts = unresolved(src)
    # rerere.autoupdate stages every hunk it replays; nothing left means done.
    if not conflicts and git(src, *IDENT, "commit", "--no-edit", check=False).returncode == 0:
        return None
    git(src, *IDENT, "merge", "--abort", check=False)
    return f"conflicts in {', '.join(conflicts)}" if conflicts else result.stderr.strip()


def apply_file(src: Path, patch: Path) -> str | None:
    """`git am -3` one patch file; None on success, else why it was dropped."""
    result = git(src, *IDENT, "am", "-3", str(patch), check=False)
    if result.returncode == 0:
        return None
    conflicts = unresolved(src)
    if not conflicts and git(src, *IDENT, "am", "--continue", check=False).returncode == 0:
        return None
    # Without an identity (CI has none) `am --abort` fails and leaves the
    # conflict behind for every patch after it.
    git(src, *IDENT, "am", "--abort", check=False)
    return f"conflicts in {', '.join(conflicts)}" if conflicts else result.stderr.strip()


def rr_cache(src: Path) -> Path:
    common = Path(out(src, "rev-parse", "--git-common-dir"))
    return (common if common.is_absolute() else src / common) / "rr-cache"


def stamp_version(src: Path, stamp: str, label: str) -> str:
    cargo = src / "Cargo.toml"
    text = cargo.read_text(encoding="utf-8")
    found = re.search(
        r'(?ms)^\[workspace\.package\]\r?$.*?^version = "(\d+)\.(\d+)\.(\d+)(?:-[^"]*)?"(?=\r?$)', text
    )
    if not found:
        sys.exit("no [workspace.package] version in Cargo.toml")
    major, minor, patch = (int(part) for part in found.groups())
    # A prerelease of the next patch sorts above the release it was built on.
    version = f"{major}.{minor}.{patch + 1}-{label}.{stamp}"
    line_start = text.rfind("\n", 0, found.end()) + 1
    cargo.write_text(text[:line_start] + f'version = "{version}"' + text[found.end():], encoding="utf-8")
    subprocess.run(["cargo", "update", "--workspace", "--quiet"], cwd=src, check=True)
    git(src, "add", "Cargo.toml", "Cargo.lock")
    # apps/zgx is a workspace of its own whose lock records the root's crates
    # it uses by path, at the root's version.
    zgx = src / "apps" / "zgx"
    if (zgx / "Cargo.lock").is_file():
        subprocess.run(["cargo", "update", "--workspace", "--quiet"], cwd=zgx, check=True)
        git(src, "add", "apps/zgx/Cargo.lock")
    git(src, *IDENT, "commit", "-q", "-m", f"nightly: version {version}")
    return version


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--src", type=Path, required=True, help="dedicated git checkout to assemble in")
    parser.add_argument("--config", type=Path, default=TOOLS / "patches.toml",
                        help="patch list to assemble (patches.toml; patches-v3.toml for v3)")
    parser.add_argument("--stamp", default=dt.datetime.now(dt.timezone.utc).strftime("%Y%m%d%H%M"))
    parser.add_argument("--report", type=Path, help="write release notes (Markdown) here")
    parser.add_argument("--json", type=Path, help="write the run summary (JSON) here")
    parser.add_argument("--keep-workflows", action="store_true", help="keep upstream's .github/workflows")
    parser.add_argument("--save-rerere", action="store_true",
                        help="copy conflict resolutions recorded in --src into rerere/ and exit")
    args = parser.parse_args()
    src = args.src.resolve()
    sys.stdout.reconfigure(encoding="utf-8")

    if args.save_rerere:
        shutil.copytree(rr_cache(src), TOOLS / "rerere", dirs_exist_ok=True)
        print(f"saved resolutions to {TOOLS / 'rerere'}; commit them")
        return

    config = tomllib.loads(args.config.read_text(encoding="utf-8"))
    upstream = config["upstream"]
    upstream_url = f"https://github.com/{upstream['repo']}.git"
    nightly_url = f"https://github.com/{config['nightly']['repo']}.git"
    patches = [p for p in config.get("patch", []) if p.get("enabled", True)]

    for op in ("merge", "am", "rebase", "cherry-pick"):
        git(src, *IDENT, op, "--abort", check=False)
    git(src, "config", "rerere.enabled", "true")
    git(src, "config", "rerere.autoupdate", "true")
    if (TOOLS / "rerere").is_dir():
        shutil.copytree(TOOLS / "rerere", rr_cache(src), dirs_exist_ok=True)

    refspecs = [f"+refs/heads/{upstream['branch']}:refs/nightly/upstream"]
    refspecs += [f"+refs/pull/{p['pr']}/head:refs/nightly/pr/{p['pr']}" for p in patches if "pr" in p]
    git(src, "fetch", "--no-tags", "--quiet", upstream_url, *refspecs)
    branches = [f"+refs/heads/{p['branch']}:refs/nightly/branch/{p['branch']}" for p in patches if "branch" in p]
    if branches:
        git(src, "fetch", "--no-tags", "--quiet", nightly_url, *branches)

    base = out(src, "rev-parse", "refs/nightly/upstream")
    git(src, "checkout", "-q", "-f", "-B", "nightly", base)
    git(src, "clean", "-fdq")
    run = Run(inputs=[f"upstream {base}"])

    for patch in patches:
        name = patch["name"]
        note = f" — {patch['note']}" if patch.get("note") else ""
        if "pr" in patch:
            number, sha = patch["pr"], patch.get("sha", "")
            link = f"[#{number}](https://github.com/{upstream['repo']}/pull/{number})"
            if not sha:
                run.dropped.append(f"**{name}** ({link}): no pinned `sha`")
                continue
            info = github(f"repos/{upstream['repo']}/pulls/{number}")
            if info and info.get("merged"):
                run.notices.append(f"{link} is merged upstream: remove **{name}** from patches.toml")
                continue
            if info and info.get("state") == "closed":
                run.notices.append(f"{link} was closed without merging; still applied at the pin")
            head = out(src, "rev-parse", f"refs/nightly/pr/{number}")
            if head != sha:
                run.notices.append(f"{link} has new commits since the pin (head `{head[:9]}`); review, then move `sha`")
            if git(src, "cat-file", "-e", f"{sha}^{{commit}}", check=False).returncode != 0:
                git(src, "fetch", "--no-tags", "--quiet", upstream_url, sha)
            if git(src, "merge-base", "--is-ancestor", sha, "HEAD", check=False).returncode == 0:
                run.notices.append(f"{link} at `{sha[:9]}` is already in upstream: remove **{name}**")
                continue
            failure = merge(src, sha, f"nightly: merge {name} (#{number})")
            label, ident = f"{link} at `{sha[:9]}`", f"pr {number} {sha}"
        elif "file" in patch:
            path = TOOLS / patch["file"]
            failure = apply_file(src, path)
            digest = hashlib.sha256(path.read_bytes()).hexdigest()
            label, ident = f"`{patch['file']}`", f"file {patch['file']} {digest}"
        elif "branch" in patch:
            tip = out(src, "rev-parse", f"refs/nightly/branch/{patch['branch']}")
            failure = merge(src, tip, f"nightly: merge {name} (branch {patch['branch']})")
            label, ident = f"branch `{patch['branch']}` at `{tip[:9]}`", f"branch {patch['branch']} {tip}"
        else:
            sys.exit(f"patch {name!r} needs one of pr, file or branch")
        if failure:
            run.dropped.append(f"**{name}** ({label}): {failure}")
        else:
            run.included.append(f"**{name}** — {label}{note}")
            run.inputs.append(ident)

    if not args.keep_workflows and (src / ".github" / "workflows").is_dir():
        # Upstream's CI belongs to zeronsh/zeron; here it would only run and fail.
        git(src, "rm", "-rq", ".github/workflows")
        git(src, *IDENT, "commit", "-q", "-m", "nightly: drop upstream workflows")

    version = stamp_version(src, args.stamp, config["nightly"].get("label", "nightly"))
    fingerprint = hashlib.sha256("\n".join(run.inputs).encode()).hexdigest()[:16]
    subject = out(src, "log", "-1", "--format=%s (%cs)", base)
    summary = {
        "version": version,
        "upstream": base,
        "head": out(src, "rev-parse", "HEAD"),
        "fingerprint": fingerprint,
        "dropped": len(run.dropped),
    }

    lines = [
        f"Upstream: [{upstream['repo']}@{base[:9]}](https://github.com/{upstream['repo']}/commit/{base}) — {subject}",
        "",
        "### Included",
        *(f"- {item}" for item in run.included or ["nothing beyond upstream"]),
    ]
    if run.dropped:
        lines += ["", "### Dropped from this build", *(f"- {item}" for item in run.dropped)]
    if run.notices:
        lines += ["", "### Notices", *(f"- {item}" for item in run.notices)]
    lines += ["", f"<!-- fingerprint: {fingerprint} -->"]
    report = "\n".join(lines) + "\n"

    if args.report:
        args.report.write_text(report, encoding="utf-8")
    if args.json:
        args.json.write_text(json.dumps(summary, indent=2), encoding="utf-8")
    if github_output := os.environ.get("GITHUB_OUTPUT"):
        with open(github_output, "a", encoding="utf-8") as sink:
            for key, value in summary.items():
                sink.write(f"{key}={value}\n")
    print(report)
    print(json.dumps(summary, indent=2))


if __name__ == "__main__":
    main()
