# kraai

`kraai` is an agentic runtime for safe, token-efficient LLM execution.
Models use Nushell directly instead of serializing named tool calls.

## Features

- **Nushell scripts**: One complete script can compose external commands,
  structured pipelines, and native Kraai commands without JSON argument payloads.
- **Capability sandbox**: Profiles grant filesystem, metadata, network, or
  no-sandbox capabilities and independently decide whether escalation is denied,
  prompted, or allowed.
- **Stateful commands**: `kraai-open-files`, `kraai-close-files`, and
  `kraai-edit-file` update durable context while the script is running.
- **Crash-safe execution**: Exact source, stdout, stderr, completed state effects,
  and terminal status are persisted for recovery.
- **Dynamic system prompt**: Current pinned-file contents and workspace
  instructions are refreshed for each model request.

### Script protocol

An assistant may write ordinary progress text and then one script block:

```xml
<tool_call timeout="30sec" permissions="workspace-write">
let packages = cargo metadata --no-deps --format-version 1 | from json
$packages.packages | select name version
</tool_call>
```

The runtime executes the block as one Nushell script and returns one
`<tool_call_result>` block. The model must choose a timeout. Capability requests
are evaluated for the whole script before it starts; successfully completed
commands and state effects remain completed if a later statement fails.

### Context Cost

`kraai-open-files` keeps a file out of script-result history. The runtime reads the
current on-disk contents and injects that current snapshot into future model
requests until `kraai-close-files` removes it. A model can use ordinary Nushell
commands such as `open`, `ls`, or an external `rg` when it needs immediate data
instead of durable context.

Use this simplified model:

- `F`: rendered token size of the file
- `n`: edits to the file
- `m`: extra non-edit model requests after the last edit
- `c`: cached input token multiplier, for example `0.1` when cached tokens cost
  10% as much as uncached tokens

This ignores tool schemas, instructions, edit payloads, output tokens, and file
size changes.

The context-window difference is simple. A conventional read/edit loop before
each edit leaves `n` full file snapshots in the conversation before the final
answer:

```text
read-loop context = n * F
open-file context = F
```

The billing-weighted input-token model is different because old read results can
be cached. Across `n` edits plus the final answer, each read result is paid once
as new input and then remains in cached history for later requests:

```text
read-loop = F * (n + c * n^2)
open-file = F * (n + 1)
```

So without extra follow-up calls:

```text
read-loop / open-file = (n + c * n^2) / (n + 1)
```

At `c = 0.1`, open-file becomes cheaper on weighted input tokens at 4 edits:

```text
1 edit:    read-loop is 0.55x the open-file cost
2 edits:   read-loop is 0.80x the open-file cost
3 edits:   read-loop is 0.98x the open-file cost
4 edits:   read-loop is 1.12x the open-file cost
10 edits:  read-loop is 1.82x the open-file cost
20 edits:  read-loop is 2.86x the open-file cost
50 edits:  read-loop is 5.88x the open-file cost
100 edits: read-loop is 10.89x the open-file cost
```

If the file stays open and unchanged for `m` more requests, prompt caching makes
the follow-up term:

```text
read-loop = F * (n + c * n^2 + c * m * n)
open-file = F * (n + 1 + c * m)
```

With no cache hits for the injected open-file snapshot, use `m` instead of
`c * m` in the open-file formula. Closing the file after the edit batch removes
that follow-up cost entirely.

```text
open-file wins when c * n^2 + c * m * (n - 1) > 1
```

## Usage

Run in the terminal
```bash
kraai
```

Run through nix
```bash
nix run github:kraai-io/kraai
```

Build with cargo
```bash
cargo run
```

## Configuration

Most configuration can be done through the UI. Currently all configuration is unstable and might change at any time.

## Evaluations

`kraai-eval` runs agent harnesses against immutable Git fixtures, captures their
submissions, and grades those submissions in a fresh workspace with tests that
were never mounted in the agent sandbox. Results and process logs are stored by
experiment identity for exact reuse. See [docs/evaluations.md](docs/evaluations.md)
for the task format, isolation boundary, and current limitations.

## License

Apache-2.0
