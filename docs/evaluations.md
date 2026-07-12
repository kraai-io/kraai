# Agent evaluations

`kraai-eval` runs a command-based agent harness against an exported Git tree and
grades the resulting patch in a different workspace. The agent receives the
task prompt and public source tree only. Hidden tests, grader commands, source
history after the selected revision, and cached results are never mounted in
the agent sandbox.

The evaluator supports individual attempts and cache-aware suites. It records
task correctness, wall-clock time, provider request and token usage, optional
harness-local metrics, and complete failure artifacts.

## Run the first Kraai task

The repository includes an evaluation derived from the parent of Jujutsu change
`nrozvoul`. It asks the harness to make the open-file and close-file tool APIs
plural, then grades the submitted patch with tests that were not present in the
agent workspace.

Pass a model ID available through the `openai` Codex subscription provider:

```bash
just eval-open-close-files <model-id>
```

The recipe builds immutable Nix artifacts for both Kraai and `kraai-eval`, uses
the current `~/.kraai/providers.toml`, starts the subscription proxy, and runs
the `build-code` profile. Supply a different provider ID or stochastic attempt
number as the second or third argument:

```bash
just eval-open-close-files <model-id> openai 1
```

Run three attempts and print an aggregate success-rate, timing, and token
summary with:

```bash
just eval-open-close-files-suite <model-id> openai 3 0
```

Suites always reuse matching immutable attempt results, so an interrupted suite
can be rerun without paying for completed attempts.

Results and full process logs are stored under `.kraai-eval-cache/`. Re-running
the same attempt refuses to overwrite it; increment the attempt, or use the
generic CLI's `--reuse-result` flag to inspect an existing result.

## Task layout

Keep public and private task inputs beside the manifest. Paths may not escape
the task directory. The source repository itself may be elsewhere because only
the selected tree is exported into a newly initialized Git repository.

```text
tasks/example/
  task.toml
  public.patch
  hidden.patch
```

```toml
schema_version = 1
id = "example-fix"
prompt = "Fix the reported behavior. Add useful tests."
max_submission_bytes = 10485760

[source]
repository = "/path/to/source-repository"
revision = "4a0abd13abc1a81fdb706a572ae5f2ceba603e29"
# public_patch = "public.patch"

[runner]
timeout_seconds = 600
network = "disabled"
# Mount an offline Cargo registry and the host's Nix Rust development tools.
rust_toolchain = true
# Enforced for the runner and hidden graders through a transient user cgroup.
max_memory_bytes = 8589934592
max_processes = 512
cpu_quota_percent = 400

[grader]
hidden_patch = "hidden.patch"

[[grader.commands]]
command = ["cargo", "nextest", "run", "-p", "example", "hidden_regression"]
timeout_seconds = 300
```

The revision is resolved to a full commit before hashing or execution. `git
archive` exports only that tree, so the agent cannot recover a later fix from
the source repository's object database. A public patch can add task setup that
the agent is allowed to inspect. A hidden patch is applied only after the agent
has stopped.

## Running a harness

The command runner accepts an exact executable plus an explicit version label.
The executable's SHA-256 digest is authoritative; the label can be a release
version, Git commit, Nix flake reference, or another human-readable identifier.

```bash
cargo run -p kraai-eval -- run \
  --task tasks/example/task.toml \
  --runner-program /nix/store/.../bin/agent \
  --runner-version git:4a0abd13abc1a81fdb706a572ae5f2ceba603e29 \
  --harness-name example-agent \
  --model-label example-model \
  --runner-arg run \
  --runner-arg --workspace \
  --runner-arg '{workspace}' \
  --runner-arg --prompt \
  --runner-arg '{prompt}' \
  --openai-proxy \
  --attempt 0
```

`{workspace}`, `{prompt}`, `{proxy_url}`, and `{provider_config}` are replaced
without shell evaluation. Arguments remain argv items, including when the
prompt contains spaces or shell syntax.

While a run is active, the CLI displays an elapsed timer with the task,
harness/version, model label, attempt, and current phase. Interactive terminals
update one status line in place. Redirected output emits one line per phase
change so CI logs remain readable.

When the run finishes, the CLI prints a compact human-readable summary with the
result, runner and grader outcomes, elapsed time, and artifact directory. Full
structured details remain in that directory's `result.json`; they are not
dumped to the terminal.

Every result includes absolute start and completion timestamps. Once an
experiment identity has been resolved, controller/setup errors are committed as
`controller_failed` results with the failing phase, error chain, event log, and
a pointer to the retained work directory. Earlier launch errors, such as an
invalid manifest or unavailable credential, are written under
`.kraai-eval-cache/failures/<id>/failure.json` before the CLI returns the error.

## Metrics and suites

The credential proxy counts requests, statuses, total request time, and token
usage returned by OpenAI Responses or Chat Completions APIs. It normalizes
cached input and reasoning output tokens into separate fields.

Harnesses can additionally write a JSON report to the path in
`KRAAI_EVAL_METRICS_PATH`. The path is a dedicated writable file outside the
submission workspace, so the report cannot become part of the submitted patch:

```json
{
  "schema_version": 1,
  "turns": 3,
  "tool_calls": 8,
  "final_context_tokens": 42000,
  "usage": {
    "total_tokens": 50000,
    "input_tokens": 30000,
    "output_tokens": 9000,
    "reasoning_tokens": 6000,
    "cache_read_tokens": 5000
  }
}
```

All fields after `schema_version` are optional. Proxy usage is authoritative
when available; the harness report supplies internal measurements such as turn,
tool-call, and final-context counts. Kraai's `--ci` mode writes this report
automatically.

The generic `suite` command accepts one or more `--task` arguments plus
`--attempts` and `--start-attempt`. It reuses cached attempt results, prints a
human-readable summary, and saves the complete run list, success rate, failure
counts, wall-time distribution, and token distributions in `summary.json`.
Success rate uses evaluated attempts as its denominator; controller and launch
failures are reported separately rather than counted as task failures.

## Credential proxy

`--openai-proxy` starts a trusted reverse gateway for OpenAI-compatible API-key
providers. The controller reads `OPENAI_API_KEY` by default; use
`--openai-api-key-env NAME` to select a different trusted environment variable.
The gateway accepts at most 64 authenticated upstream requests per attempt by
default; adjust this with `--openai-proxy-max-requests`.
The real credential remains in the controller. The sandbox receives only a
random, short-lived gateway token and these values:

```text
OPENAI_API_KEY=<short-lived gateway token>
OPENAI_BASE_URL=http://127.0.0.1:<port>/v1
OPENAI_API_BASE=http://127.0.0.1:<port>/v1
KRAAI_EVAL_OPENAI_BASE_URL=http://127.0.0.1:<port>/v1
```

The gateway accepts only authenticated `GET` and `POST` requests to
`/v1/models`, `/v1/chat/completions`, and `/v1/responses`. It strips the sandbox
authorization header, injects the real upstream bearer credential, streams the
response back, and logs method, allowlisted path, status, and duration without
bodies or credentials. The cache identity includes a one-way credential
fingerprint so changing accounts or keys cannot reuse an old result.

Kraai's generic OpenAI chat-completions provider can use this gateway through
`{proxy_url}` and the default `OPENAI_API_KEY` environment variable.

For subscription-backed Codex, use `--codex-subscription-proxy`. The trusted
controller loads the existing Kraai OAuth state and performs refreshes. The
sandbox receives only `KRAAI_EVAL_CODEX_PROXY_TOKEN` and a sanitized provider
configuration containing one selected `openai-codex` provider and its models.
Other providers, API keys, and unrelated models are removed. The source defaults
to `~/.kraai/providers.toml`; use `--kraai-provider-config` to override it or
`--kraai-provider-id` when the file contains more than one Codex provider.

```bash
kraai-eval run \
  --task tasks/example/task.toml \
  --runner-program /nix/store/.../bin/kraai \
  --runner-version git:<full-commit> \
  --codex-subscription-proxy \
  --runner-arg --ci \
  --runner-arg --auto-approve \
  --runner-arg --provider \
  --runner-arg '<provider-id>' \
  --runner-arg --model \
  --runner-arg '<model-id>' \
  --runner-arg --agent-profile \
  --runner-arg build-code \
  --runner-arg --message \
  --runner-arg '{prompt}' \
  --runner-arg --provider-config \
  --runner-arg '{provider_config}'
```

The sanitized config is committed into the immutable task base before the agent
starts, so it does not appear in the submitted patch. The user's original config
is read only and is not modified.

Use a different explicit attempt number for each stochastic repetition. An
existing identity is never overwritten:

```bash
# Return the exact existing result without executing the harness again.
kraai-eval run ... --attempt 0 --reuse-result
```

## Isolation

Evaluations currently fail closed unless Linux bubblewrap is available. The
sandbox has:

- a cleared environment;
- a private home and temporary directory;
- a writable public workspace with read-only Git metadata;
- read-only system mounts and only the resolved harness's Nix closure;
- separate PID and user namespaces;
- no network namespace by default;
- a wall-clock timeout with process termination; and
- a bounded combined stdout/stderr capture; and
- cgroup-v2 memory, aggregate process, and CPU limits.

Tasks with `runner.rust_toolchain = true` additionally receive the current Rust
toolchain and common source-inspection commands from their immutable Nix store
closures. The Cargo registry is mounted read-only and Cargo is forced offline;
build artifacts remain inside the disposable task workspace. This exposes
downloaded public crate sources, but not Cargo credentials or Git checkouts.

The grader always runs without network access. Enabling runner network access
is recorded in the experiment identity and result. The credential proxy
prevents credential disclosure and limits what can be requested through the
gateway, but bubblewrap cannot force all traffic through that gateway. Arbitrary
commands launched by a network-enabled harness can still make unrelated
outbound connections. An OCI or VM backend with an egress firewall remains
necessary for strong network isolation.

Resource controls use `systemd-run --user` and fail closed when a delegated
user cgroup is unavailable. The defaults are configurable per task through the
runner policy shown above. It never falls back to an unbounded or unsandboxed
execution.

## Submission and hidden grading

After the harness exits, the controller stages all workspace changes and writes
a binary Git patch subject to `max_submission_bytes`. It then:

1. copies the pristine public base to a new grading workspace;
2. applies the captured submission;
3. applies the hidden grader patch; and
4. runs each grader command offline.

The grader does not reuse the agent workspace. This prevents surviving agent
processes, modified Git metadata, ignored files, or poisoned build state from
becoming part of the result. Agent-authored tests remain in the submission and
can run alongside hidden tests.

For Rust tasks, prefer black-box verifiers or hidden integration tests. A hidden
patch can add an internal `#[cfg(test)]` module when private behavior must be
tested, but those patches require more care because they can conflict with a
valid implementation.

## Cache and logs

The experiment identity includes:

- public task and private grader digests;
- resolved runner artifact digest and version label;
- resolved Rust development-program store paths when requested;
- runner arguments;
- network policy; and
- explicit attempt number.

The source repository's local path is deliberately excluded. Its resolved Git
commit identifies the exported public tree, so moving the same corpus checkout
does not invalidate otherwise identical cached results.

Completed runs are written atomically under a human-readable hierarchy:

```text
.kraai-eval-cache/runs/<task>/<harness>/<version>/<model>/attempt-<n>/<experiment-id>/
```

Suite summaries use the same useful coordinates:

```text
.kraai-eval-cache/suites/<harness>/<version>/<model>/<suite-id>/summary.json
```

The exact relative directory is also returned as `artifact_path` in the result.
Each completed attempt contains the manifest, structured result, append-only
event stream, runner and grader stdout/stderr, harness metrics, credential-proxy
events when enabled, and the submitted patch. Controller failures contain the
same artifacts produced before the error and retain their disposable work tree
for diagnosis.

The sandbox environment is cleared, but harness output can still contain data
returned by a model or printed by the harness. Do not inject long-lived secrets
into runner arguments or the public workspace. Output redaction remains
necessary before accepting untrusted third-party task corpora.
