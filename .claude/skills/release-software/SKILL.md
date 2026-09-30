---
name: release-software
description: Cut an official, tagged release of croit AIplane using the date-based YYMM.RELEASE.BUILD version scheme. Use when the user wants to "make a release", "release the software", "tag a version", "cut 2609.x", or ship an official version. Drives version derivation → verify the target commit is pushed and CI-green → annotated git tag → push the tag (triggers the tag pipeline that publishes images, chart, GitHub Release, and moves :production). A release is a tag and nothing else — no version commit.
---

# Release croit AIplane

Cut a tagged release. The authority is `docs/releases.md` — read it if anything
below is ambiguous, and prefer it over this file if the two disagree. This skill
is the procedure.

Scheme recap: **`YYMM.RELEASE.BUILD`** (e.g. `2609.2.0`). Releases always end in
`.0`; main builds auto-number `YYMM.RELEASE.<commits-since-tag>` via
`scripts/derive-version.sh`. You never touch build numbers — you only derive
`YYMM.RELEASE` and create the tag.

🔴 **A release is a tag and nothing else.** No version commit, no file to bump.
`Cargo.toml`'s `version`, `Chart.yaml`, the Dockerfile — none of them is edited.
If you find yourself preparing a `chore(release)` commit, stop.

## 0. Hard gates — stop if any fails

- 🔴 **Never cut a release unprompted** (AGENTS.md). The user must have asked for
  a release *in this session*, in so many words. "Ship/finish X" is not a release
  request. Invoking `/release-software` *is* the ask.
- Releases are cut from `main` only, from a commit that is **already on
  `origin/main`** — never from unpushed local work.
- Working tree clean. A dirty tree cannot leak into the tag, but it usually means
  work is in flight — ask.

If any gate is unmet: report it and stop.

## 1. Pick the target commit and prove it is green

Default target: `origin/main`'s HEAD. If the user names a sha, use that — and tag
**that exact sha**, never `HEAD`.

```bash
git fetch origin main --tags --quiet
git status --short
git rev-parse HEAD origin/main       # the release candidate (should match)
gh run list --branch main --limit 20 \
  --json databaseId,headSha,status,conclusion,workflowName,url \
  | jq -c '.[] | select(.workflowName == "CI")'
```

⚠️ Never use `gh run list --workflow …` here, neither `CI` nor `ci.yml`: both
have returned a run from weeks earlier while the current one was in progress,
which makes a green HEAD look untested or an untested one look green. Filter
on `workflowName` in the output instead.

The run for the exact target sha must be `completed` / `success`.
- **Running** → wait: `gh run watch <id> --exit-status`. Never tag on a guess.
- **Red** → stop and report. Do not quietly tag an older green commit; ask first.

## 2. Compute the version number — do NOT ask the user

🔴 **The number is derived, not chosen.** Never ask the user to confirm it and
never offer alternatives. State it in one line and continue.

```bash
YYMM=$(date +%y%m)
NEXT=$(git tag -l "v$YYMM.*" --sort=-v:refname | head -1 \
        | sed -E "s/^v$YYMM\.([0-9]+)\..*/\1/")
echo "v$YYMM.$(( ${NEXT:-0} + 1 )).0"
```

Sanity rules — a violation means you mis-derived, so recompute rather than ask:
- `YYMM` is **today's** month, never a future one.
- The first release of a month is `.1`; never reuse or skip a number.
- `git tag -l 'v<computed>'` must be empty.

## 3. Tag the verified commit + push the tag

Annotated, `v`-prefixed (the workflow triggers on `v*`, the resolver rejects
anything not `vX.Y.Z`). Pass the sha explicitly:

```bash
git tag -a v2609.3.0 <green-sha> -m "Release 2609.3.0"
git push origin v2609.3.0
```

Push **only** the tag, never the branch alongside it.

🔴 Pushing the tag **moves `:production`**, which every installation with
`autoUpdate.enabled` (the chart default) picks up at its next nightly restart.
The user's release request in this session covers the push; a release request
from an earlier session does not.

## 4. Watch the tag pipeline

```bash
gh run list --limit 10 --json databaseId,headBranch,workflowName,status,url \
  | jq -c '.[] | select(.workflowName == "CI" and .headBranch == "v2609.3.0")'
gh run watch <id> --exit-status
```

The tag pipeline re-runs the **full** suite (lint, tests, release build) and only
publishes if green — so a red tag build ships nothing. Last, it creates the
GitHub Release entry. Expect ~20–30 minutes; run the watch in the background.

A red **`chrome extension`** job alone is expected while an earlier extension
submission is still in Web Store review: the store refuses the next upload, but
every image and the chart are already published. Report it, and re-run the job
once the review clears (`gh run rerun <id> --failed`). See
`docs/browser-control.md`.

## 5. Verify

```bash
helm show chart oci://ghcr.io/croit/charts/aiplane    # no --version → must be the new one
gh release view v2609.3.0                              # entry exists, notes read sensibly
GITHUB_REF_TYPE=tag GITHUB_REF_NAME=v2609.3.0 sh scripts/derive-version.sh
```

Also confirm `:v<version>` and `:production` exist for all four images
(`ghcr.io/croit/aiplane`, `-sandbox`, `-sandbox-runner`, `-ocr-sidecar`).

## 6. Report

Version, commit, pipeline URL, GitHub Release URL — and, explicitly, that
`:production` moved, so auto-updating installations pick it up tonight. A release
is not a quiet event.

Then **offer** (don't post unasked) a short user-facing summary of what changed:

```bash
git log --oneline --merges    v<prev>..v<new>   # merge subjects = feature summaries
git log --oneline --no-merges v<prev>..v<new>
```

Rewrite for a normal user: drop issue numbers, file names, dependabot bumps and
pure chore/CI/test commits unless users would notice them. English, bullets only.

## Recovery — failed tag pipeline

Only safe *before* anyone pulled the images:

```bash
git tag -d v2609.3.0
git push origin :refs/tags/v2609.3.0
# fix on main, wait for green, redo from step 1
```

Otherwise cut the next number.
