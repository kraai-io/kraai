# Sandbox support

Kraai uses Bubblewrap on Linux, Seatbelt on macOS, and AppContainer on Windows.

Windows requires no administrator setup, background service, or additional driver.
Workspace read/write permissions, private temporary files,
and process cleanup are enforced by the Windows backend.

Windows has two limitations:

- `host-read` and `host-write` are unsupported. Requests using them fail before
  starting a process. Use explicit runtime roots for additional read access.
- `network` enables AppContainer network capabilities, but localhost connections
  remain blocked. This differs from Linux and macOS.

Runtime roots on Windows must permit Kraai to update their access-control entries.
Each workspace gets one AppContainer identity per access level. Its grants on the
workspace and runtime roots are inheritable and kept, so only the first run in a
workspace walks its files; files moved in later with their own explicit permissions
are not covered.
`no-sandbox` disables sandbox enforcement; Kraai never selects it automatically
when a capability is unsupported.

The workspace, every runtime root that receives sandbox access, and the private
temporary directory must sit on a filesystem with persistent ACLs and
open-by-file-ID support. Windows reports both through the
[volume capability flags](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-getvolumeinformationw).
NTFS has both. exFAT has no persistent ACLs. On an unsupported filesystem startup
fails, naming the workspace and the affected path. UNC sandbox roots are
unsupported.

The workspace comes from the session's persisted `workspace_dir`, not from where
`kraai.exe` lives, so moving the executable does not move an existing session.
Move the workspace to a supported filesystem and update it through
`RuntimeHandle::set_workspace_dir`, or start a new session from the new
directory.

On macOS, Kraai terminates the execution's process group on completion, timeout,
cancellation, or dropped execution. Processes that deliberately detach into another
process group or session can survive. Seatbelt filesystem and network restrictions
remain attached to those processes.
