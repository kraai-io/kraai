# Sandbox support

Kraai uses Bubblewrap on Linux, Seatbelt on macOS, and AppContainer on Windows.

Windows requires no administrator setup, background service, or additional driver.
Workspace read/write permissions, metadata protection, private temporary files,
and process cleanup are enforced by the Windows backend.

Windows has two limitations:

- `host-read` and `host-write` are unsupported. Requests using them fail before
  starting a process. Use explicit runtime roots for additional read access.
- `network` enables AppContainer network capabilities, but localhost connections
  remain blocked. This differs from Linux and macOS.

Runtime roots on Windows must permit Kraai to update their access-control entries.
`no-sandbox` disables sandbox enforcement; Kraai never selects it automatically
when a capability is unsupported.
