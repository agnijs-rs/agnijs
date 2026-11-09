# Issue tracker: Backlog.md

Issues and tasks for this repo are tracked with [Backlog.md](https://github.com/MrLesk/Backlog.md). Tasks are markdown files under `backlog/`, managed through the `backlog` CLI. GitHub Issues are not used.

## Conventions

- Run `backlog instructions overview` at the start of a conversation, and read the matching `backlog instructions <guide>` before task lifecycle actions (see `AGENTS.md`).
- **Create an issue**: `backlog task create "Title"` (see `backlog instructions task-creation` for description, acceptance criteria and flags).
- **Read an issue**: `backlog task <id> --plain`
- **List / search issues**: `backlog task list --plain`, `backlog search "<query>" --plain`
- **Update an issue** (status, notes, criteria): `backlog task edit <id> ...`
- **Close an issue**: set status to Done with `backlog task edit <id> -s Done`, or archive with `backlog task archive <id>`.
- Never edit task files in `backlog/` by hand; use the CLI.

## When a skill says "publish to the issue tracker"

Create a Backlog.md task with `backlog task create`.

## When a skill says "fetch the relevant ticket"

Run `backlog task <id> --plain`.
