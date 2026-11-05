# Claude Loop Runner

[![CI](https://github.com/alphathekiwi/claude-loop-runner/actions/workflows/ci.yml/badge.svg)](https://github.com/alphathekiwi/claude-loop-runner/actions/workflows/ci.yml)
[![codecov](https://codecov.io/gh/alphathekiwi/claude-loop-runner/graph/badge.svg)](https://codecov.io/gh/alphathekiwi/claude-loop-runner)

**Run Claude Code across many files in parallel. Every change is verified, retried if it fails, and committed on its own.**

Give it a list of files and one prompt ("add tests for this file"). It runs one Claude CLI per file, checks each result with your command, asks Claude to fix failures, and saves progress so you can stop and resume at any time.

**Jump to:** [Quick start](#quick-start) · [How it works](#how-it-works) · [Safety](#safety-by-design) · [Recipes](#recipes) · [CLI reference](#cli-reference) · [Placeholders](#placeholders) · [Troubleshooting](#troubleshooting)

---

## Quick start

**You need:** Rust 1.85+ and the [`claude` CLI](https://docs.anthropic.com/en/docs/claude-code), installed and logged in. `tmux` and `git` are optional.

1. **Build it**
   ```bash
   cargo install --path .
   ```
2. **List the files to work on** in `files.json` (the values are passed to Claude as context; `null` is fine):
   ```json
   { "src/utils/parser.ts": null, "src/utils/formatter.ts": null }
   ```
3. **Run it**
   ```bash
   claude-loop-runner \
     --input files.json \
     --prompt "Add unit tests for this file" \
     --verify "npx vitest run {file_stem}"
   ```
4. **Watch it.** If `tmux` is installed, the runner moves itself into a background tmux session and prints how to attach:
   ```bash
   tmux attach -t <session-name>
   ```

> [!IMPORTANT]
> **On Linux, add `--limit 0`.** The usage monitor reads your Claude login from the macOS Keychain. Elsewhere it logs an error every 2 minutes and never pauses.

---

## How it works

```
files.json ──► Prompt pool ──► Verify pool ──pass──► commit (optional) ──► ✅ completed
               claude -p        --verify
                                   │ fail
                                   ▼
                              Fixup: claude gets the error output,
                              then verify runs again (up to --max-retries)
                                   │ still failing
                                   ▼
                              ❌ failed, log in <tasks-dir>/failures/<file>.log
```

Two worker pools connected by channels, with all progress saved to disk so a run can stop and resume.

- **Prompt pool**: each worker runs one Claude CLI instance on one file. A non-zero exit marks the file failed. Claude is told which files it may touch (`--allowlist`); with git enabled, edits outside that are logged as warnings.
- **Verify pool**: runs `--verify` with every substituted path shell-quoted, so agent-created file names can't inject commands. On failure it re-prompts with `--fixup`, up to `--max-retries` fixups, then marks the file failed and writes a log under `<tasks-dir>/failures/`.
- **Resumable state**: every file moves through an explicit [state machine](#task-states) persisted to JSON. `Ctrl-C` kills in-flight Claude and verify processes and leaves their files resumable; `--resume` re-queues them, and flags passed with it are saved into the task. `--dry-run` writes the task without running it so it can be reviewed and edited first.
- **Backpressure**: pauses workers above 85% system memory (resumes below 70%), and when Claude usage passes `--limit` on the 5-hour or 7-day window, sleeping until that window resets.
- **Git safety**: workers share a git lock and commit only named paths, so commits never pick up another file's changes, anything you had staged, files dirty before the run, or the runner's own state. One commit per completed file, with or without `--verify`.

---

## Safety by design

Running an AI agent unattended over a real codebase means planning for what goes wrong. Each row is a failure the runner is built to prevent, and each has a test.

| What could go wrong | What the runner does |
|---|---|
| Claude names a file `x;rm -rf ~;.ts` and your verify command runs it | Every path substituted into `--verify` is **shell-quoted** |
| Claude crashes, and the file is counted as done | A **non-zero exit** marks the file failed, with the error saved |
| You press `Ctrl-C`, and Claude keeps editing with nobody watching | In-flight Claude and verify processes are **killed**; their files resume next run |
| Two workers commit at once and mix up each other's changes | A **shared git lock**, and each commit names its exact files |
| Your own uncommitted work ends up in an AI commit | Files dirty before the run, and anything you staged, are **never committed** |
| A run burns through your Claude quota | Workers **pause** above `--limit` and sleep until the quota resets |

> [!NOTE]
> **The allowlist is guidance, not a sandbox.** Claude is told the limit in its prompt. Edits outside it produce a warning (with git enabled), and auto-commit only takes files whose path contains the target's name. Nothing stops Claude from writing elsewhere, so run it on a clean branch.

---

## Recipes

### Review a task before running it

```bash
# 1. Create the task without running anything
claude-loop-runner --input files.json --prompt "Add tests" --verify "npm test -- {file_stem}" --dry-run

# 2. Edit it: prompt, verify command, retries, or remove files
$EDITOR ./claude-loop-tasks/tasks/state_0.json

# 3. Run it
claude-loop-runner --resume task_0
```

`--resume` with no ID picks the first unfinished task. Flags you pass with `--resume` (e.g. `-c 2`) are saved into the task for next time.

### Commit each file as it passes

```bash
claude-loop-runner --input files.json --prompt "Add JSDoc comments" \
  --verify "npm run lint {file}" --git-commit
```

Each passing file becomes its own commit, `claude-loop: parser.ts`. Add `--git-branch` to put them on a new `claude-loop/<task>-<timestamp>` branch.

> [!TIP]
> **Using GitButler?** Skip `--git-branch`; it creates a normal git branch that conflicts with virtual branches. Commits from `--git-commit` land in your workspace, ready to organise.

### Linux, CI, or no tmux

```bash
claude-loop-runner --input files.json --prompt "..." --limit 0 --no-tmux
```

---

## CLI reference

**Required for a new task:** `-i, --input <FILE>` and `-p, --prompt <TEXT>`.

| Prompting and verifying | | Default |
|---|---|---|
| `-v, --verify <CMD>` | Command that must pass for a file to complete. See [placeholders](#placeholders) | none |
| `-f, --fixup <TEXT>` | Prompt sent with the error output when verify fails | "Fix the issues with the file" |
| `--max-retries <N>` | Fixup attempts per file before it is marked failed | 3 |
| `-a, --allowlist <PATTERN>` | Files Claude is told it may edit (not enforced) | `{file_stem}*` |
| `--claude-bin <PATH>` | Claude binary to run | `claude` |

| Scale and limits | | Default |
|---|---|---|
| `-c, --concurrency <N>` | Parallel prompt workers | 5 |
| `--verify-concurrency <N>` | Parallel verify workers | same as `-c` |
| `-m, --max-files <N>` | Process at most N files this run | all |
| `--limit <PERCENT>` | Pause above this Claude usage. `0` turns it off | 95 |

| Git | | Default |
|---|---|---|
| `--git` | Record dirty files first and warn about edits outside the allowlist | off |
| `--git-commit` | Commit each completed file (implies `--git`) | off |
| `--git-branch` | Create a task branch first (implies `--git`) | off |
| `--git-commit-message <TEXT>` | Appended to commits: `claude-loop: parser.ts (<TEXT>)`. Not a template | none |
| `--no-git` | Turn every git feature off for this run | off |

| Running and resuming | | Default |
|---|---|---|
| `--resume [TASK_ID]` | Continue a task, or the first unfinished one | |
| `--dry-run` | Create the task without running it | |
| `-w, --working-dir <DIR>` | Where Claude and verify run | current dir |
| `-d, --tasks-dir <DIR>` | Where task state is stored | `./claude-loop-tasks` |
| `--no-tmux` | Don't relaunch inside tmux | |

---

## Placeholders

### In `--verify` (every value is shell-quoted)

| Placeholder | For `src/utils/parser.ts` |
|---|---|
| `{file}` | `src/utils/parser.ts` |
| `{file_stem}` | `parser` (`.test`/`.spec` are stripped too) |
| `{file_dir}` | `src/utils` |
| `{all_files}` | the file plus everything matching the allowlist |
| `{test_files}` | allowlist matches that look like tests (`.test.`, `.spec.`, `_test.`, `/tests/`, `/__tests__/`) |
| `{created_files}` | allowlist matches other than the file itself, e.g. new test files |

The list placeholders are resolved in `--working-dir`, space-separated and sorted.

### In `--allowlist`

`{file}`, `{file_stem}` and `{file_dir}` only, as a glob. A pattern without a `/` is matched in the file's own directory, so `{file_stem}*` covers `parser.ts`, `parser.test.ts`, and so on.

---

## Running in tmux

If you're not already in tmux, aren't using `--dry-run` or `--no-tmux`, and `tmux` is installed, the runner:

1. Creates the task.
2. Starts itself again in a detached tmux session with `--resume <task>`.
3. Prints `Attach with: tmux attach -t <session>` and exits.

The session keeps running if your terminal closes. To stay in the foreground, pass `--no-tmux`.

---

## Task states

```
pending → prompt_in_progress → awaiting_verification → verify_in_progress → completed
                                                              │
                                                    fails ──► fixup_in_progress ──► verify again
                                                              │
                                                 out of retries ──► failed
```

Everything lives in the tasks directory, and is saved after every status change:

```
claude-loop-tasks/
├── task_list.json        # every task, and whether it finished
├── tasks/state_0.json    # one task: config + every file's status
└── failures/parser.ts.log  # verify output and fixup prompts for failed attempts
```

---

## Troubleshooting

<details>
<summary><b>The command printed a tmux session name and exited</b></summary>

That's the [tmux relaunch](#running-in-tmux). Attach with the printed `tmux attach -t …`, or rerun with `--no-tmux`.
</details>

<details>
<summary><b>"Failed to fetch usage" every 2 minutes</b></summary>

The usage monitor needs the macOS Keychain. Pass `--limit 0` on Linux or in CI.
</details>

<details>
<summary><b>It asks for a git name and email</b></summary>

Commits need `user.name` and `user.email`. Enter them (saved to this repo only), choose to disable git for the run, or set them yourself with `git config`. Without a terminal, git features are turned off automatically.
</details>

<details>
<summary><b>A file is marked failed</b></summary>

Read `<tasks-dir>/failures/<file>.log`: it holds each verify output, the fixup prompt sent, and Claude's reply. To retry it, fix the cause, then in the state file set its `"status"` to `"pending"` and `"attempts"` to `0`, and `--resume`.
</details>

---

## Development

```bash
cargo test                                   # unit + end-to-end tests
cargo clippy --all-targets -- -D warnings
cargo llvm-cov --summary-only                # coverage
```

End-to-end tests run the real binary against a scripted fake Claude CLI (`tests/fixtures/fake-claude.sh`), so no API calls are made.
