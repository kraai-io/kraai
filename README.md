<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/brand/svg/kraai-logo-white.svg">
    <img alt="kraai" src="assets/brand/svg/kraai-logo-black.svg" width="360">
  </picture>
</p>

Kraai is a next-gen (coding) harness.

It keeps the model's context minimal: a short system prompt, a few tools, and
nothing the model doesn't need.

---

Current priorities, in order:

1. **Efficiency.** Big wins so far.
   - Nushell scripts. One script can search a repo, filter the matches, and
     edit a file, where other agents need several tool calls.
   - Open files. A file the model is working on stays in context as one copy
     that Kraai re-reads from disk, instead of a new snapshot after every edit.
2. **Features.** Catching up.
   - MCP servers, including OAuth sign-in.
   - Agent Client Protocol, so ACP editors can use Kraai as their agent.
   - Skills, web search, and image input.
3. **Safety.** Solid base.
   - Every script runs in a sandbox (Bubblewrap, Seatbelt, or AppContainer)
     and asks for the access it needs. Anything beyond the agent profile
     prompts you first.
4. **UX.** Barely started.
   - Open as many clients as you want at once, in terminals or ACP editors,
     on the same sessions. Every client follows the same live stream and
     approval prompts, whichever one is running the turn.
   - `/continue` picks a session back up from where it stopped, on whichever
     model you have selected.

---

## Terminal-Bench 4

Kraai and Codex on the same 20 Terminal-Bench 4 tasks, both running
`gpt-6-astra` at low reasoning effort:

| | Passed | Input tokens / trial | Output tokens / trial | Cost / trial |
| --- | --- | --- | --- | --- |
| Kraai | 36 / 40 | 486k | 6.7k | $1.20 |
| Codex 0.151.0 | 94 / 100 | 1.63M | 13.2k | $2.96 |

Kraai passes about as many tasks while reading 70% fewer input tokens and
writing half the output, for 40% of the cost.

Some caveats. These are the 20 tasks Codex finds easiest, so the subset says
nothing about the hard half of the benchmark. Codex passes 94% of them but 51%
of all 66. Kraai ran each task twice. The Codex numbers come from
its [published leaderboard run](https://hub.harborframework.com/jobs/a538f69d-7756-444a-9dfe-43a0ab10b3a8),
filtered to the same tasks. Input tokens include cache reads, and cost is an
estimate at API prices ($10/M input, $1/M cached input, $50/M output).

---

## CPU and memory

Each agent's terminal UI, started fresh in the same repo and left idle:

| | Memory (RSS) | CPU, first 30 s | CPU while idle |
| --- | --- | --- | --- |
| Kraai | 42 MB | 0.17 s | 0.07% |
| Codex 0.162.0 | 470 MB | 2.2 s | 0.8% |
| Claude Code 2.1.295 | 320 MB | 3.4 s | 0.2% |

Kraai uses a tenth of Codex's memory, a small fraction of its startup CPU, and
about a tenth of its idle CPU.

Each number is the median of three runs on Linux, in a 200×50 tmux pane, with
the idle CPU measured over 60 seconds. Only each agent's own processes count;
MCP servers it starts are left out.

---

## Run it

```sh
nix run github:kraai-io/kraai
```

Run it from the directory you want to work in. Add a provider with
`/providers`, pick a model with `/model`, and type a task. `/help` lists the
rest.

---

## Development

```sh
nix develop
just run
```

Run `just check` before pushing.

### Evals

`kraai-eval` runs an agent on a task and grades the result. Local tasks in
`evals/tasks` are Git repos with hidden tests the agent never sees, and
`benchmark` runs public suites like Terminal-Bench 4 through
[Harbor](https://harborframework.com). Every model request goes through a local
proxy that records tokens and estimated cost. Interrupted runs resume where
they stopped. It needs Linux, plus Docker for public suites.

```sh
just eval list
just eval run event-stream --model gpt-6-astra-low
just eval benchmark terminal-bench --model gpt-6-astra-low --task-count 20
just eval-view    # results in the browser, next to published Codex runs
```

### Performance

`kraai-perf` times fixed offline workloads, such as startup, streaming a reply,
and running a Nushell script, without calling a model. It reports wall time,
CPU time, and peak memory, so you can compare a change against a saved
baseline.

```sh
just perf run --output before.json
just perf compare before.json after.json
just profile cpu --workload streaming-response    # flame graph
```

---

## License

Apache-2.0
