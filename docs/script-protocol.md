# Script protocol

The assistant can write a progress update followed by one script block:

```xml
<tool_call timeout="30sec" permissions="workspace-write">
let packages = cargo metadata --no-deps --format-version 1 | from json
$packages.packages | select name version
</tool_call>
```

The runtime runs the block as one Nushell script and returns one
`<tool_call_result>` block. The model must choose a timeout. Before execution,
the runtime checks capability requests for the whole script. If a statement
fails, earlier commands and state changes are not rolled back.

