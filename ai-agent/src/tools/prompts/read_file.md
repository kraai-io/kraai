## read_file

Description: Request to read the contents of one file. The tool outputs line-numbered content (e.g. \"1 | const x = 1\") for easy reference when creating diffs or discussing code.

**IMPORTANT: You can read a maximum of one file in a single read_file request.** If you need to read more files, use multiple read_file requests.

Parameters:

- path: (required) File path (relative to workspace directory)

Usage:

```tool
{"name":"read_file","parameters":{"path":"path/to/file"}}
```

Examples:

1. Reading a file:

```tool
{"name":"read_file","parameters":{"path":"src/app.ts"}}
```

The response will look like this:
read_file src/app.ts

```
1 | this is the first line
```
