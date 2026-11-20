# Benchmark-backed homepage examples

These are selected, single-attempt examples from the existing local coding evals on 2026-09-29. Kraai and the regular Codex CLI used the same `gpt-5.6-sol` model at low reasoning, the original task prompt, one attempt, and a 16-request proxy ceiling. Both submissions passed each task's hidden grader. [Chart-ready data](benchmark-homepage-examples.json) includes the exact usage breakdown and paths to the saved run summaries in `.kraai-eval-cache`. The [comparison results and harness profiles](benchmark-evidence) are included for review.

| Task | Kraai tokens | Codex tokens | Token reduction | Kraai estimated API cost | Codex estimated API cost | Cost reduction |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| [Incremental event decoder](benchmark-evidence/event-stream.json) (`event-stream`) | 46,866 | 125,859 | 62.8% | $0.157560 | $0.288988 | 45.5% |
| [Asset router repair](benchmark-evidence/repository-repair.json) (`repository-repair`) | 63,425 | 107,680 | 41.1% | $0.142478 | $0.203888 | 30.1% |
| [Dependency scheduler](benchmark-evidence/dependency-waves.json) (`dependency-waves`) | 49,335 | 105,110 | 53.1% | $0.174310 | $0.164779 | **5.8% higher** |
| **Three-task total** | **159,626** | **338,649** | **52.9%** | **$0.474348** | **$0.657655** | **27.9%** |

Recommended homepage lead: **“Both agents passed. Kraai used 53% fewer tokens across three coding tasks.”** A nearby caption can add: “Estimated API-equivalent cost was 28% lower across these runs.” Keep the scope (“three tasks, one run each”) visible.

Two focused example cards:

- **Build an incremental event decoder — 63% fewer tokens.** Both agents passed the hidden tests for split UTF-8, line endings, event fields, and error handling. Kraai used 46.9k tokens versus Codex's 125.9k; estimated API-equivalent cost was 45% lower. Average input context per model request was 5.4k versus 13.3k tokens.
- **Repair an asset router — 41% fewer tokens.** Both agents passed the hidden tests for mount selection, inherited settings, and decoded path safety. Kraai used 63.4k tokens versus 107.7k; estimated API-equivalent cost was 30% lower. Average input context per request was 8.6k versus 13.0k tokens.

For a third card focused on context size, the dependency scheduler used 53% fewer total tokens and 51% less input context per model request (5.6k versus 11.4k). Its estimated cost was 5.8% **higher** for Kraai, so this card should not claim a cost saving.

For the homepage chart, use paired horizontal bars for total tokens on all three tasks and an optional cost toggle that shows all three costs, including the scheduler exception. Put a “Both passed” badge beside each pair. The data JSON has uncached input, cached input, output, reasoning, model-request counts, and context sizes for a detailed view.

These are examples, not an estimate of average performance: each task has one completed attempt per harness, and the tasks were selected for illustration. The proxy logs confirm that every successful model request used `gpt-5.6-sol` at low reasoning. Total tokens include cached input. Costs are estimates of API-equivalent pricing, not charges to the Codex subscription. Timing depends on the machine and is omitted from the suggested copy. The comparison excludes an earlier Codex attempt whose sandbox lacked its code-mode helper and earlier Kraai attempts stopped by tighter request caps; those attempts produced no valid paired score. The completed runs use Kraai commit `90397ece6dcad24df26ea59fd86ab984043a1e58` and Codex CLI `0.159.0`.
