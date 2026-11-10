# Running Miri locally, nightly

CI runs the collector tests under Miri with the loops shrunk (`cfg!(miri)` in
`crates/agnijs-js-vm/tests/gc.rs`), one job per test, so the job stays well
under an hour. `tools/miri-local.mjs` runs the same suite at full size
(`AGNIJS_MIRI_FULL=1`) on a spare machine and reports through the `gh` CLI.

## What the script does

1. Clones or updates a clean checkout of `origin/main` outside the repository
   (default `C:/Users/Jason/.t3/projects/agnijs-miri-run`; `--dir` changes it).
2. Runs `cargo +nightly miri test --locked -p agnijs-js-vm --test gc` there,
   one process, with `MIRIFLAGS=-Zmiri-disable-isolation`, a target directory
   under the output directory, and an overall timeout of 3 hours
   (`--timeout-min`). `--include-ignored` also runs the tests that are
   `#[ignore]`d under Miri (hours; not for nightly use).
3. Writes the log to `<dir>-out/miri-gc.log`.
4. On failure: opens, or comments on, the issue "Miri failure on main"
   (label `ci`, created if missing) with the commit, the failing test names
   and the last 80 log lines, and exits 1.
5. On success: comments on the issue "Miri status" (created and pinned if
   missing) only when 7 or more days have passed since the last green comment.
   The date is kept in `<dir>-out/state.json`.

`--dry-run` prints what would be posted and changes nothing on GitHub or in the
state file. `--skip-run` skips the checkout and the Miri run and judges the
existing log, which is useful together with `--dry-run`.

Requirements: `git`, `gh` (authenticated, with permission to open issues on
agnijs-rs/agnijs), Node.js, and the nightly toolchain with the `miri` and
`rust-src` components (`rustup component add --toolchain nightly miri rust-src`).

## Scheduling with Windows Task Scheduler

Nightly at 03:00, as the current user, when logged on or not:

```
schtasks /Create /TN "AgniJS Miri nightly" /SC DAILY /ST 03:00 /TR "node C:\Users\Jason\.t3\projects\agnijs\tools\miri-local.mjs" /F
```

Remove it with `schtasks /Delete /TN "AgniJS Miri nightly" /F`; run it once now
with `schtasks /Run /TN "AgniJS Miri nightly"`. The script is read from the
agnijs working copy, but it only runs the checkout it makes itself, so the
state of your working copy never affects the result. Leave the machine's other
heavy work off the 03:00 slot: the full run takes a long time and is a single
process.

## Alternative: a self-hosted GitHub runner

If the machine hosts a runner, a workflow can do the same job. The rule is
strict: a self-hosted runner executes whatever code the workflow checks out,
so never trigger it from `pull_request` (or any event a fork or an untrusted
branch can cause). Only `schedule` and `workflow_dispatch`, on `main`:

```yaml
on:
  schedule:
    - cron: "0 3 * * *"
  workflow_dispatch:
jobs:
  miri-full:
    if: github.ref == 'refs/heads/main'
    runs-on: [self-hosted, windows, miri]
    timeout-minutes: 180
    permissions:
      issues: write
    env:
      GH_TOKEN: ${{ github.token }}
    steps:
      - uses: actions/checkout@v4
      - run: node tools/miri-local.mjs --dir "$env:RUNNER_TEMP/miri-run"
```

Register the runner with a dedicated label (`miri`) and keep the repository's
"Run workflows from fork pull requests" approval setting on.
