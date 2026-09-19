# Contributing to QBZ

This project is actively evolving. Contributions are welcome, but we have a few rules to keep releases stable and avoid regressions (especially around audio output).

## Where the code lives

The live app is the Rust workspace under `crates/` — a single native process
with a Qt/QML interface. UI code and the desktop binary live in
`crates/qbz-qt`; shared behavior belongs in the relevant core crate.

The Slint crates (`crates/qbz-ui`, `crates/qbz`, `crates/qbz-slint-common` and
`crates/qbz-dac-wizard`) are frozen historical references. The old Svelte
`src/` and Tauri `src-tauri/` trees were deleted in 2.0.2 and survive only at
the git tag `legacy-tauri-svelte`. PRs against these retired paths cannot be
merged — port the change to the live Qt/Rust tree instead.

## Quick rules

- Write clear, concise English (no emojis in code, comments, or commit messages).
- Keep PRs focused and small when possible.
- Disclose every LLM model used and its role in the PR body; see below.
- Do not change app branding or legal disclaimers without discussing it first.
- Do not modify protected audio-backend behavior unless explicitly requested by the maintainer.

## Branch naming

We use a consistent branch naming scheme:

`<type>/<origin>/<branch_name>`

- `type`: `feature` | `bugfix` | `hotfix` | `refactor` | `release` | `chore` | `docs`
- `origin`:
  - `internal`: created/owned by maintainers
  - `external`: branches/commits authored by third-party contributors (PRs)

Examples:

- `feature/internal/offline-cache-encryption`
- `bugfix/internal/login-footer-alignment`
- `docs/internal/contributing-process`
- `feature/external/add-album-to-playlist`

## Branch workflow

We use a **pre-release integration branch** to keep `main` stable and release-ready at all times.

```
feature/xyz ──┐
bugfix/abc  ──┼──> pre-release ──> main (tagged release)
hotfix/123  ──┘
```

### Branch hierarchy

1. **`main`** - Releases ONLY. Protected branch. Merging here triggers a tagged release.
2. **`pre-release`** - Integration branch. All features and fixes merge here first.
3. **`feature/*`, `bugfix/*`, etc.** - Individual work branches.

### For contributors

**All PRs must target `pre-release`, not `main`.**

PRs targeting `main` will be closed and asked to retarget to `pre-release`.

### Procedure (maintainer)

1. **Triage**
   - Confirm scope and that it does not touch protected areas (audio routing/backends, credential storage, etc.) unless requested.
   - Verify PR targets `pre-release` (not `main`).
2. **Check out the PR**
   - `gh pr checkout <PR_NUMBER>`
3. **Rename the checked-out branch (local)**
   - Use an `external` branch name so it's obvious these commits are third-party authored:
   - `git branch -m <type>/external/<topic>`
4. **Merge to pre-release**
   - `git checkout pre-release`
   - `git merge --no-ff <type>/external/<topic>`
5. **Run checks**
   - Test or check each touched core crate from `crates/`.
   - For Qt/QML changes, run the relevant QML audits and use
     `./scripts/qt-run.sh` from the repository root when your platform can
     build the desktop application.
   - Never build the frozen Slint crates as validation for a live change.
6. **Push pre-release**
   - `git push origin pre-release`
7. **Close the PR with a comment** explaining it was merged to `pre-release`.

### Releasing to main

When ready to release:

```bash
git checkout main
git merge pre-release
git push origin main
git tag vX.Y.Z
git push origin vX.Y.Z
```

This is done exclusively by maintainers.

### Merge strategy note (to preserve “external” authorship)

If you want the git history to clearly show third-party authored commits, avoid “squash merge”.
Prefer:

- **Create a merge commit**, or
- **Rebase and merge** (preserves individual commits/authors)

## What to include in PRs

- A short description of the problem and solution.
- For a bug fix: the observed behavior, the root cause or best-supported
  diagnosis, and why the proposed change addresses it.
- For a feature: the user need, the existing flow or components you inspected,
  and the constraints the design preserves.
- Screenshots for UI changes when possible.
- The checks or manual tests you ran.
- The LLM disclosure described below, or `None` if no LLM was used.
- Notes about any breaking changes or migrations.

## AI / LLM-assisted contributions

LLM-assisted contributions are welcome. If you used one or more models, the
PR body must identify each model as precisely as the tool exposes it and state
what role it played. A product, client or provider name alone is not enough
when the tool shows a more specific model name or version. There is
deliberately no predefined model list to keep current: report the identifier
shown by the tool at the time of the contribution.

List different models separately when they handled different stages, such as
repository/context mapping, problem definition, root-cause analysis,
plan/contract/dependency graph, implementation, tests or code review. Include
only the stages that apply and use one line per model:

```text
- <provider and model/version shown by the tool>: repository mapping, diagnosis
- <provider and model/version shown by the tool>: plan/contract, implementation
- <provider and model/version shown by the tool>: tests, code review
```

Saying only that a change was "prompted" or "AI-generated" does not describe
the engineering process. The PR should make it possible to see how the problem,
existing code, constraints and proposed solution were understood before the
change was submitted.

If the exact model is not shown by the tool, say that instead of guessing.
Prompt transcripts and private conversations are not required. This disclosure
helps the maintainer choose an appropriate review strategy; it is not grounds
for rejecting a contribution. The contributor remains responsible for reading
the resulting diff and reporting how it was verified.

### Authorship and responsibility

The human who signs a commit is its author and is responsible for it. Models are
tools: they are disclosed in the PR body, as described above, and they are not
added as co-authors. Do not include `Co-Authored-By` lines for a model, and do
not attribute a change to a product or an agent. Credit here follows
accountability, and an agent cannot answer a review comment six months from now.

Before opening a PR, read the whole diff yourself and run it. "The model says it
works" is not a test result, and a change verified only by the same agent that
wrote it has not been reviewed — it has been re-read by its author.

The same rule applies to review and testing:

- **Findings are welcome, verified or not** — open an issue describing what you
  observed, with logs or steps. Say plainly if an agent found it and you have
  not reproduced it. That is useful and honest.
- **Fixes must be verified by you.** Explain what you ran and what you saw.
- **A generated code review is not a review.** If you submit one, check each item
  against the current code first and drop what does not hold. An unfiltered list
  of possible problems moves the work of separating real from imaginary onto the
  maintainer, and that is the opposite of a contribution.

## What not to include

- Large refactors mixed with feature work.
- Changes that reintroduce removed UI/UX patterns (for example, exporting offline cache files).

---

## Internationalization (i18n)

QBZ ships 8 locales (`en es de fr pt ru ja nl`) as gettext `.po` files, bundled
via the `qbz-i18n` crate. Rules:

- **No hardcoded UI strings in QML** — use
  `QbzSession.tr("…", QbzSession.trRev)` so live language changes update the
  binding. Rust uses `qbz_i18n::t()` or `qbz_i18n::tf()`.
- Adding or changing a string means updating **all** locale `.po` files, not
  just English.
- The English text is the gettext `msgid`; do not introduce dotted keys.

### Checklist for PRs with UI Text

- [ ] No hardcoded strings in QML — all text uses `QbzSession.tr()` with
      `trRev`
- [ ] Every new/changed string updated across all 8 `.po` locales
- [ ] Reused an existing string where one already fit
