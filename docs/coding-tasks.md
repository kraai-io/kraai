# Coding tasks: Kraai and Codex CLI

Run on 2026-09-29 with Kraai commit `90397ece6dcad24df26ea59fd86ab984043a1e58`
and Codex CLI 0.159.0. Both used `gpt-5.6-sol` at low reasoning effort, the
original task prompt, one attempt per task, and a 16-request proxy cap. The
proxy logs show every successful model request used that model and effort.

Both agents passed the hidden grader on all three tasks:

- [`event-stream`](coding-tasks/event-stream.json) builds an incremental event
  decoder. The grader covers split UTF-8, line endings, event fields, and error
  handling.
- [`repository-repair`](coding-tasks/repository-repair.json) repairs an asset
  router. The grader covers mount selection, inherited settings, and decoded
  path safety.
- [`dependency-waves`](coding-tasks/dependency-waves.json) builds a dependency
  scheduler. Kraai used fewer tokens here but cost more, because it wrote
  more output tokens.

[`coding-tasks.json`](coding-tasks.json) has the per-task token breakdown,
request counts, context sizes, costs, and the paths of the saved run summaries
in `.kraai-eval-cache`. The harness profiles are
[`kraai-compare16.toml`](coding-tasks/kraai-compare16.toml) and
[`codex-compare16.toml`](coding-tasks/codex-compare16.toml).

The tasks were picked as examples, and one attempt per task says nothing about
the average. Token totals include cached input. Costs are API-equivalent
estimates, not what the Codex subscription charged.

The comparison leaves out an earlier Codex attempt whose sandbox lacked its
code-mode helper, and earlier Kraai attempts that hit tighter request caps.
None of those produced a valid paired score.
