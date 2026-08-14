---
name: schedule
description: >-
  Guided setup of a recurring headless Claude task via the cc-loadout CLI —
  turn "every morning do my weekly report" into a scheduled `cc-loadout task`.
  Use whenever a cc-loadout user wants to schedule, automate, or set up a
  recurring/daily Claude job — set a prompt to run on a cron-like schedule,
  "每天早上幫我…", "排程一個 task", "run /cortex:weekly every day",
  "recurring claude task" — even if they don't say "cc-loadout" or
  "/cc-loadout:schedule" by name. Drives the `cc-loadout task` CLI (which
  validates and writes everything); you never hand-write tasks.json.
---

# Guided cc-loadout task setup

You help the user create a recurring headless Claude task via `cc-loadout` — the
config that schedules `claude -p <prompt>` to run non-interactively at a fixed
time, on a chosen account, inside a specific repo. You drive the `cc-loadout task`
CLI; **you do not hand-write `tasks.json` or touch crontab yourself** — the CLI
validates and writes everything.

## Prerequisite

The `cc-loadout` CLI must be installed (`cc-loadout --version`). If it's missing,
that's normally the SessionStart hook not having run yet or having failed — have
the user start a fresh session first; `install.sh` (see the cc-loadout README) is
the fix only for someone using the CLI without the plugin.

## Steps

### 1. Shape the prompt (the real value-add)

Get the recurring intent from the user and turn it into a concrete, self-contained
headless prompt. The key constraint: `claude -p` runs **non-interactively** — there
is no follow-up, no clarification round-trip, no keyboard. The prompt must be fully
self-contained.

Prefer a slash command where one exists (`/cortex:weekly`, `/cortex:distill`,
`/code-review`, etc.) — they are already self-contained by design. Otherwise write a
crisp imperative that leaves no ambiguity (e.g. `Generate the weekly Cortex report and
commit it` rather than `do the weekly thing`).

Ask the user what they want to happen, then draft the prompt text with them before
moving on.

### 2. Pick when, who, and where

Three things to resolve:

**When** — one or more `HH:MM` times (24-hour). Multiple fire times are passed as
separate `--at` values (e.g. `--at 08:00 --at 20:00`).

**Who (account)** — list real account aliases and have the user pick one:
```
cc-loadout account list --json
```
Use the `alias` field from the JSON output. The task runs under that account's
Claude session.

**Where (cwd)** — an **absolute** path to the repo the task should run in.
`cc-loadout` does not expand `~`, so confirm the full path (e.g.
`/home/you/cortex`, not `~/cortex`). This is also where any relative slash
commands resolve their context.

**Profile (optional)** — if the task needs specific plugins active (e.g. a cortex
skill), pass `--profile <name>`. Skip if the default global loadout is sufficient.

**Model (optional)** — `--model <alias-or-name>` (e.g. `haiku`, `sonnet`, `opus`,
`claude-sonnet-4-6`). Omit it and the task inherits whatever the `claude` CLI resolves
by itself, which is the account's default tier — so a daily task can quietly eat the
most expensive model. Pin it when the job is mechanical enough for a cheaper tier, or
when it genuinely needs the strongest one. Primes are always forced onto `haiku`
unless they pin something else — a heartbeat has no reason to bill the top tier.

### 3. Create the task

Choose a short kebab-case id that describes the job (e.g. `weekly`, `morning-digest`,
`daily-review`). Then run:

```
cc-loadout task add <id> \
  --account <alias> \
  --at <HH:MM> \
  --prompt "<prompt text>" \
  --cwd <absolute-path> \
  [--profile <profile-name>] \
  [--model <alias-or-name>]
```

Multiple fire times: repeat `--at`:
```
cc-loadout task add weekly \
  --account work \
  --at 08:00 \
  --prompt "/cortex:weekly" \
  --cwd /home/you/cortex
```

Relay any CLI error verbatim and stop — do not retry blindly. Fix the argument that
caused the error and re-run.

### 4. Test once (optional but recommended)

Fire the task immediately to confirm the prompt runs as expected:
```
cc-loadout task run <id>
```

Then verify it persisted and is scheduled:
```
cc-loadout task list --json
```

Look for the task's `id` as a key in the `tasks` object (e.g. `.tasks["weekly"]`;
there is no `"id"` field) and confirm its `times` (and `account`, `cwd`, `prompt`)
are what you intended. For a readable view with the computed next fire time, use
`cc-loadout task list` without `--json`.

### 5. Retrieve results

`cc-loadout task run` runs headless. After it completes, reopen the session to see
what Claude produced:
```
cc-loadout task resume <id>
```

This reopens the most recent run's session in the standard Claude Code interface.

## Prime vs task

If the user only wants to keep an account's 5-hour context window warm — no actual
work, just a heartbeat — that is a **prime**, not a task. Use:
```
cc-loadout account schedule set <alias> <HH:MM> [<HH:MM>...]
```
A prime has no `--prompt`; it just opens and closes a lightweight session to refresh
the window, and it runs on `haiku` so the heartbeat costs as little as possible. Use
`task add --prompt` only when there is real work to do.

## Boundaries

- **Don't set up accounts here.** If the user has no account yet, defer to the
  existing account onboarding flow and come back when `cc-loadout account list`
  shows at least one alias.
- **Never write `tasks.json` directly.** `task add` is the only correct path —
  it validates input and writes atomically.
- **One task per `task add`.** Each invocation creates one task; run it once per
  distinct scheduled job.
