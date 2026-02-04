# Claude Loop Runner

A parallel task runner that orchestrates multiple Claude CLI instances to process files concurrently. Designed for batch operations like adding tests, refactoring code, or applying consistent changes across many files.

## Installation

```bash
cargo build --release
```

## Quick Start

The runner takes a JSON input file mapping file paths to metadata, and a prompt describing the task:

```bash
claude-loop-runner \
  --input files.json \
  --prompt "Add comprehensive unit tests for this file" \
  --verify "npm test -- {file_stem}" \
  --concurrency 5
```

## Input File Format

The input JSON maps file paths to metadata that gets passed to Claude:

```json
{
  "src/utils/parser.ts": {
    "description": "Parses configuration files",
    "exports": ["parseConfig", "validateConfig"]
  },
  "src/utils/formatter.ts": {
    "description": "Formats output strings",
    "exports": ["formatDate", "formatCurrency"]
  }
}
```

## Usage Examples

### Example 1: Dry Run, Edit, and Resume

Use `--dry-run` to create a task configuration without executing it. This lets you review and edit the task before running.

**Step 1: Create the task**

```bash
claude-loop-runner \
  --input files-to-test.json \
  --prompt "Write unit tests for this file using vitest" \
  --verify "npx vitest {file_stem} --run" \
  --fixup "The tests are failing. Fix the issues based on the error output." \
  --allowlist "{file_stem}*" \
  --concurrency 3 \
  --dry-run
```

Output:
```
2026-02-04T10:00:00Z  INFO Loaded input file input=files-to-test.json files=15
2026-02-04T10:00:00Z  INFO Created new task task_id=task_0
2026-02-04T10:00:00Z  INFO Dry run complete - task created but not executed
2026-02-04T10:00:00Z  INFO To run this task, use: claude-loop-runner --resume task_0
```

**Step 2: Review and edit the task state**

The task configuration is saved in `./claude-loop-tasks/state_0.json`. You can edit this file to:
- Modify the prompt
- Change verification command
- Remove files you don't want processed
- Adjust max retries

```bash
# View the task state
cat ./claude-loop-tasks/state_0.json | jq .

# Edit if needed
vim ./claude-loop-tasks/state_0.json
```

**Step 3: Resume the task**

```bash
# Resume the specific task
claude-loop-runner --resume task_0

# Or resume the first incomplete task
claude-loop-runner --resume
```

### Example 2: Running with Git Integration

Git integration helps track changes when running parallel workers. It captures pre-existing dirty files so they don't trigger false "unauthorized change" warnings, and can auto-commit completed files.

**Basic git tracking:**

```bash
claude-loop-runner \
  --input files.json \
  --prompt "Refactor this file to use async/await" \
  --verify "npm run typecheck" \
  --git \
  --concurrency 10
```

This will:
1. Capture all dirty files before starting (e.g., your working changes)
2. Build a global allowlist of all files being processed
3. Only warn about truly unauthorized changes (files outside the task scope)

**With auto-commit:**

```bash
claude-loop-runner \
  --input files.json \
  --prompt "Add JSDoc comments to all exported functions" \
  --verify "npm run lint {file}" \
  --git \
  --git-commit \
  --concurrency 5
```

Each file that passes verification gets committed automatically:
```
2026-02-04T10:05:00Z  INFO Verification PASSED worker=2 file=src/utils/parser.ts
2026-02-04T10:05:00Z  INFO Auto-committed changes worker=2 file=src/utils/parser.ts commit=a1b2c3d
```

**GitButler compatibility:**

If you use GitButler, avoid `--git-branch` as it creates traditional git branches that conflict with GitButler's virtual branch system. Use `--git --git-commit` instead - commits will land in your current workspace and you can organize them into virtual branches afterward.

## CLI Reference

| Option | Description | Default |
|--------|-------------|---------|
| `-i, --input <FILE>` | Input JSON file mapping filepaths to metadata | Required |
| `-p, --prompt <TEXT>` | Main prompt for Claude CLI | Required |
| `-f, --fixup <TEXT>` | Prompt used when verification fails | None |
| `-v, --verify <CMD>` | Verification command (`{file}`, `{file_stem}`, `{file_dir}` substituted) | None |
| `-c, --concurrency <N>` | Number of parallel workers | 5 |
| `-m, --max-files <N>` | Maximum files to process | All |
| `-a, --allowlist <PATTERN>` | Files Claude is allowed to modify | `{file_stem}*` |
| `-d, --tasks-dir <DIR>` | Directory for task state files | `./claude-loop-tasks` |
| `-w, --working-dir <DIR>` | Working directory for execution | Current dir |
| `--resume [TASK_ID]` | Resume a task (specific ID or first incomplete) | - |
| `--max-retries <N>` | Maximum fixup attempts per file | 3 |
| `--dry-run` | Create task without executing | - |
| `--git` | Enable git tracking (capture dirty files) | - |
| `--git-branch` | Create a branch for this task | - |
| `--git-commit` | Auto-commit after each file passes verification | - |
| `--git-commit-message <TPL>` | Custom commit message template | - |

## Pattern Substitution

The following placeholders are substituted in `--verify`, `--allowlist`, and `--git-commit-message`:

| Placeholder | Example Input | Result |
|-------------|--------------|--------|
| `{file}` | `src/utils/parser.ts` | `src/utils/parser.ts` |
| `{file_stem}` | `src/utils/parser.ts` | `parser` |
| `{file_dir}` | `src/utils/parser.ts` | `src/utils` |

## Task States

Files progress through these states:

```
Pending → PromptInProgress → AwaitingVerification → VerifyInProgress → Completed
                                       ↓
                              FixupInProgress (on failure, loops back to verify)
                                       ↓
                                    Failed (after max retries)
```

## State Files

Tasks are persisted in the tasks directory:

```
claude-loop-tasks/
├── task_list.json      # Registry of all tasks
├── state_0.json        # State for task_0
├── state_1.json        # State for task_1
└── ...
```

State is saved after every status change, so you can safely interrupt with Ctrl+C and resume later.
