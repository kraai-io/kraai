# File context and token cost

`kraai-open-files` pins a file in context without copying its contents into
script-result history. Before each model request, the runtime reads the file
from disk and includes the latest contents. This continues until
`kraai-close-files` removes the pin. For a one-time read, the model can use
Nushell commands such as `open` and `ls`, or an external command such as `rg`.

Each pin keeps the filesystem scope authorized at creation. The runtime reads
workspace pins relative to the workspace root, so replacement symlinks cannot
make it read outside the workspace. Creating a host pin requires `host-read`.
Atomic file replacement keeps a pin active. If a file goes missing, the runtime
removes its pin and reports it in the next model request.

To compare token costs, assume:

- `F`: rendered token size of the file
- `n`: edits to the file
- `m`: extra non-edit model requests after the last edit
- `c`: cached input token multiplier, for example `0.1` when cached tokens cost
  10% as much as uncached tokens

This ignores tool schemas, instructions, edit payloads, output tokens, and file
size changes.

Reading the full file before each edit leaves `n` snapshots in the conversation
before the final answer. Pinning the file keeps one:

```text
read-loop context = n * F
open-file context = F
```

Billing also depends on caching. Across `n` edits and the final answer, each
read result costs the full input rate once, then the cached rate on later
requests. With these assumptions, the weighted input-token costs are:

```text
read-loop = F * (n + c * n^2)
open-file = F * (n + 1)
```

Without extra follow-up calls, the cost ratio is:

```text
read-loop / open-file = (n + c * n^2) / (n + 1)
```

At `c = 0.1`, pinning becomes cheaper at 4 edits:

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

If the file stays open and unchanged for `m` more requests, and those requests
hit the prompt cache:

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

