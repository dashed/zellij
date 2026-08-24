# CLAUDE.md

## Picking the right models for workflows and subagents

Rankings, higher = better. Cost reflects what I actually pay (subscription
headroom and rate limits), not list price — a separate high-headroom GPT plan
makes gpt-5.5 effectively free. Intelligence is how hard a problem you can
hand the model unsupervised. Taste covers UI/UX, code quality, API design,
and copy.

| model    | cost | intelligence | taste |
|----------|------|--------------|-------|
| gpt-5.5  | 9    | 8            | 5     |
| sonnet-5 | 5    | 5            | 7     |
| opus-4.8 | 4    | 7            | 8     |
| fable-5  | 2    | 9            | 9     |

How to apply:

- These are defaults, not limits. You have standing permission to override
  them: if a cheaper model's output doesn't meet the bar, rerun or redo the
  work with a smarter model without asking. Judge the output, not the price
  tag. Escalating costs less than shipping mediocre work.
- Route each task to the **cheapest model that clears its quality bar**. Cost
  is a tie-breaker only: when it's unclear whether a cheaper model clears the
  bar for anything that ships, intelligence > taste > cost — don't trade
  correctness for price.
- Bulk/mechanical work (clear-spec implementation, data analysis, migrations):
  gpt-5.5 — it clears the bar and it's effectively free.
- Rust build/test triage (cargo build error triage, test failure analysis,
  snapshot updates): gpt-5.5 via Codex — mechanical enough to delegate; pull
  the work back to a Claude model if the fix turns architectural.
- Anything user-facing (UI, copy, API design) needs taste ≥ 7.
- Reviews of plans/implementations: fable-5 or opus-4.8, plus gpt-5.5 as an
  extra independent perspective (`codex review`).
- Never use Haiku.
- Mechanics: gpt-5.5 is only reachable through the Codex CLI — `codex exec` /
  `codex review` (my `~/.codex/config.toml` defaults to gpt-5.5 at xhigh
  reasoning effort). For investigation and data analysis run
  `codex exec -s read-only` with a self-contained prompt; for implementation
  run `codex exec -s workspace-write`; for code review run
  `codex review --base <branch>` or `codex review --uncommitted`. Capture
  just the final answer with `-o <file>` when you need programmatic output.
- ALWAYS wrap codex in a hard timeout: `timeout 900 codex exec …` for
  implementation, `timeout 300` for read-only calls. `codex exec` can hang
  forever at zero CPU on a dead network wait, and a hung exec looks identical
  to one working quietly. On timeout, retry once or do that chunk yourself —
  never wait-and-see; a zero-CPU codex process does not recover.
- Claude models (sonnet-5, opus-4.8, fable-5) run via the Agent/Workflow
  `model` parameter.

Using gpt-5.5 inside workflows and subagents (the `model` parameter only takes
Claude models, so use a wrapper):

- Spawn a thin Claude wrapper agent on the cheapest Claude model
  (`model: 'sonnet'`; in Workflow `agent()` steps also pass `effort: 'low'` —
  the Agent tool has no effort parameter) whose prompt instructs it to write
  a self-contained codex prompt, run `codex exec` via Bash, and return the
  codex output verbatim as its final message — no summarizing, no
  second-guessing.
- The codex prompt must be self-contained: codex shares none of the
  conversation's context. Include absolute file paths, the exact task spec,
  constraints, and acceptance criteria.
- The wrapper prompt must carry the timeout rule explicitly ("ALWAYS wrap
  codex in `timeout 900`; if it expires, kill it and do that chunk yourself
  or retry once") — the wrapper is the process blocked on Bash when codex
  hangs, so it cannot apply judgment after the fact.
- Give each wrapper ONE codex task, or require a checkpoint report between
  tasks — in a sequential relay, one hang stalls every task queued behind it,
  and checkpoints are how you know what was already done at takeover time.
- Pick the sandbox to match the task: `-s read-only` for
  investigation/review, `-s workspace-write` for implementation. Never
  `danger-full-access` from a subagent.
