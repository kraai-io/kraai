# kraai

`kraai` is an agentic runtime for llm tool calling.
The main goals of the project are improving token efficiency and model accuracy.

## Features

- **Toon Formatted Tool Calls**: All tool calls use less context
- **Dynamic System Prompt**: Token caching works with an ever changing system prompt
- **Stateful Tools**: Tools can cause system prompt injection with updating context every turn
- **Small Tool Set**: open_file, edit_file, bash, search_files, list_files, close_file

### Context Cost

`open_file` keeps a file out of tool-result history. The runtime reads the
current on-disk contents and injects that current snapshot into future model
requests until `close_file` removes it.

Use this simplified model:

- `F`: rendered token size of the file
- `n`: edits to the file
- `m`: extra non-edit model requests after the last edit
- `c`: cached input token multiplier, for example `0.1` when cached tokens cost
  10% as much as uncached tokens

This ignores tool schemas, instructions, edit payloads, output tokens, and file
size changes.

The context-window difference is simple. A read loop that does
`read_files -> edit_file` before each edit leaves `n` full file snapshots in the
conversation before the final answer:

```text
read-loop context = n * F
open-file context = F
```

The billing-weighted input-token model is different because old `read_files`
results can be cached. Across `n` edits plus the final answer, each read result
is paid once as new input and then remains in cached history for later requests:

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

## License

Apache-2.0
