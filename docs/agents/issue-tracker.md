# Issue tracker: Linear

Issues and specs for this repo live in **Linear**.

## Command and scope

- Command: `/home/harlan/.cache/.bun/bin/linear` (or `linear` on PATH)
- Workspace: `harlanljones`
- Team: `HJ` (Harlan Jones)
- Project: `Live trading prod readiness` — slug `741506c9d49e`, URL `https://linear.app/harlanljones/project/live-trading-prod-readiness-741506c9d49e`
  (the earlier completed phase lives in `arbkit live trading operations` — slug `ed6262e1f37c`)

Run the command's `--version` and `--help` once at the start of a tracker
session. The installed CLI's help is authoritative.

## States and labels

Linear workflow states and labels are separate. Triage roles such as
`ready-for-agent` are labels; applying one does not move workflow state unless
the invoking skill says to.

## Common operations

- Create: `linear issue create --no-interactive --team HJ --title "..." --description-file <path>`
- Read: `linear issue view [ID] --json --no-download`
- Query: `linear issue query --team HJ --all-states --all-assignees --json`
- Comment: `linear issue comment add [ID] --body-file <path>`
- Incremental labels: `linear issue update [ID] --add-label "..."` / `--remove-label "..."`
- Claim: `linear issue update [ID] --assignee self`
- Complete: `linear issue update [ID] --state completed`
- Assign to project: `linear issue update [ID] --project <projectId-or-name>`

Use Markdown files for multi-line descriptions and comments. Never print or
store the API token in the repository.

## Linear agent tracking

The production-readiness program for live trading was tracked as a parent
issue (`HJ-143`) with child tickets `HJ-144`–`HJ-153`, with native blocked-by
relations modeling the prerequisite order. All eleven issues belong to the
`Live trading prod readiness` project (slug `741506c9d49e`) and are **Done**
as of 2026-08-24; per-ticket evidence lives in each issue's closing comment,
in `GATES.md`, and in `RESULTS.md` §9.
