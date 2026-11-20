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
Every launch runs in its own AppContainer, created for that launch and deleted
when it ends. Its private temporary directory is granted to that AppContainer
alone, so concurrent launches can't read each other's temporary files.
Access to the workspace, the linked worktree's Git directory and runtime roots
goes to capabilities derived from each path and access level instead. Those
entries are inheritable and kept, so only the first launch that uses a path walks
its files. A launch only carries the capabilities for the paths its own policy
grants, so entries left on other paths give it nothing. Files moved in later with
their own explicit permissions are not covered.
Kraai deletes AppContainer profiles left behind by launches that crashed the next
time it starts a sandbox.
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

On macOS, Kraai kills the execution's process group on completion, timeout,
cancellation, or dropped execution. A process can leave that group with `setsid` or a
double fork, but it cannot drop the Seatbelt profile it inherited. Each run's profile
allows a Mach lookup of a random name no other run uses. Kraai then asks the sandbox
which processes allow that lookup, kills them, and repeats until a pass finds none.
Other runs and unsandboxed processes are not touched. If sandboxed processes are
still alive after 5 seconds, Kraai still undoes changes to `.kraai/` or `AGENTS.md`,
then fails the execution.

Sandboxed scripts cannot change the workspace's `.kraai/` directory or its
`AGENTS.md`. `.kraai/agents.toml` decides the permissions of later scripts, and
`AGENTS.md` becomes model instructions on the next turn. On Linux the existing
entries and the files their symlinks point to are mounted read-only, and on macOS
Seatbelt denies writes to them. Kraai never creates either entry itself. If a script
creates one or replaces a symlink, Kraai undoes that after the script exits, removing
a `.kraai/` it created along with everything in it. Windows has no read-only mount, so
there Kraai restores only `.kraai/agents.toml` and `AGENTS.md` after the run. Changes
to other files in a `.kraai/` that existed before the run stay on Windows. Kraai
refuses to run a write-capable sandboxed script when either entry is a symlink to a
missing path, or a symlink that reaches its target through another symlink. On Linux
and macOS it also refuses when the file has other hard links, because the script could
write through them. Ask for `no-sandbox` when a script needs to edit them.

With the `network` capability, scripts still cannot connect to Unix sockets that were
created outside the sandbox, such as D-Bus, the systemd user manager, SSH and GPG
agents, Docker, or the Nix daemon. Sockets the script creates itself keep working.
On Linux this uses Landlock ABI 9 or newer; on older kernels Kraai refuses to run
network scripts in the sandbox, and only `no-sandbox` can reach the network. On
macOS only sockets inside the private temporary directory, a writable workspace, and
the DNS resolver are reachable.
