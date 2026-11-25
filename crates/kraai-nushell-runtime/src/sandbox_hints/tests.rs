#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;

use kraai_execution_types::SandboxHint;
use kraai_sandbox::{ExecutionOutput, Termination};

use super::HintContext;

const WORKSPACE: &str = "/home/user/project";

fn context() -> HintContext {
    HintContext {
        workspace_roots: vec![PathBuf::from(WORKSPACE)],
        #[cfg(unix)]
        agent_socket: None,
    }
}

fn output(code: i32, stderr: &str) -> ExecutionOutput {
    ExecutionOutput {
        termination: Termination::Exited { code: Some(code) },
        stdout: Vec::new(),
        stderr: stderr.as_bytes().to_vec(),
        sandbox_denied: false,
        protected_entries_undone: false,
    }
}

#[test]
fn denied_writes_to_protected_entries_get_a_hint() {
    for stderr in [
        "Error: Unable to edit file\nline 1: kraai-edit-file AGENTS.md [...]\n  at columns 1 to 15: unable to write /home/user/project/AGENTS.md: Device or resource busy (os error 16)\n",
        "/bin/sh: line 1: .kraai/foo: Read-only file system\n",
        "touch: cannot touch './.kraai/agents.toml': Operation not permitted\n",
        "Error: I/O error\nline 1: 'x' | save -f notes\n  at columns 15 to 23: Read-only filesystem or storage medium\nhelp: The error occurred at '/home/user/project/AGENTS.md'\n",
        "rm: cannot remove 'AGENTS.md': Device or resource busy\n",
    ] {
        assert_eq!(
            context().hint(&output(1, stderr)),
            Some(SandboxHint::ProtectedEntry),
            "{stderr}"
        );
    }
}

#[test]
fn successful_scripts_get_no_hint() {
    let stderr = "/bin/sh: line 1: .kraai/foo: Read-only file system\n";
    assert_eq!(context().hint(&output(0, stderr)), None);
    let mut timed_out = output(1, stderr);
    timed_out.termination = Termination::TimedOut;
    assert_eq!(context().hint(&timed_out), None);
}

#[test]
fn unrelated_failures_get_no_hint() {
    for stderr in [
        "/bin/sh: line 1: /usr/share/file: Read-only file system\n",
        "grep: AGENTS.md: No such file or directory\n",
        "/bin/sh: line 1: docs/AGENTS.md: Read-only file system\n",
        "/bin/sh: line 1: /home/user/other/AGENTS.md: Read-only file system\n",
        "/bin/sh: line 1: .kraai-eval/agents.toml: Read-only file system\n",
        "/bin/sh: line 1: AGENTS.md.bak: Read-only file system\n",
        "/bin/sh: line 1: MY_AGENTS.md: Read-only file system\n",
        "cat AGENTS.md\nline two\nline three\nline four\n/usr/share/file: Read-only file system\n",
        "Error: I/O error\nline 1: open AGENTS.md; 'x' | save notes.txt\n  at columns 25 to 34: Read-only filesystem or storage medium\nhelp: The error occurred at '/home/user/project/notes.txt'\n",
    ] {
        assert_eq!(context().hint(&output(1, stderr)), None, "{stderr}");
    }
}

#[test]
fn undone_changes_get_a_hint_even_when_the_script_succeeds() {
    let mut undone = output(0, "");
    undone.protected_entries_undone = true;
    assert_eq!(
        context().hint(&undone),
        Some(SandboxHint::ProtectedEntryUndone)
    );
}

#[cfg(unix)]
struct SocketDirectory(PathBuf);

#[cfg(unix)]
impl SocketDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("kraai-hint-{}", ulid::Ulid::generate()));
        std::fs::create_dir(&path).expect("socket directory");
        Self(path)
    }

    fn socket(&self) -> (PathBuf, std::os::unix::net::UnixListener) {
        let path = self.0.join("daemon.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).expect("socket");
        (path, listener)
    }
}

#[cfg(unix)]
impl Drop for SocketDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(unix)]
fn socket_hint(context: &HintContext, socket: &Path, template: &str) -> Option<SandboxHint> {
    let stderr = template.replace("{socket}", &socket.display().to_string());
    context.hint(&output(1, &stderr))
}

#[test]
#[cfg(unix)]
fn failures_naming_a_host_socket_get_a_hint() {
    let directory = SocketDirectory::new();
    let (socket, _listener) = directory.socket();
    for template in [
        "error: cannot connect to socket at '{socket}': Permission denied\n",
        "permission denied while trying to connect to the docker API at unix://{socket}: dial unix {socket}: connect: permission denied\n",
        "curl: (7) Failed to connect to localhost:80 over unix://{socket} after 0 ms: Could not connect to server\n",
    ] {
        assert_eq!(
            socket_hint(&context(), &socket, template),
            Some(SandboxHint::HostSocket),
            "{template}"
        );
    }
}

#[test]
#[cfg(unix)]
fn failures_naming_other_paths_get_no_socket_hint() {
    let directory = SocketDirectory::new();
    let file = directory.0.join("daemon.sock");
    std::fs::write(&file, "").expect("file");
    let template = "error: cannot connect to socket at '{socket}': Permission denied\n";
    assert_eq!(socket_hint(&context(), &file, template), None);
    assert_eq!(
        socket_hint(&context(), &directory.0.join("missing.sock"), template),
        None
    );

    std::fs::remove_file(&file).expect("remove file");
    let (socket, _listener) = directory.socket();
    let inside_workspace = HintContext {
        workspace_roots: vec![directory.0.clone()],
        agent_socket: None,
    };
    assert_eq!(socket_hint(&inside_workspace, &socket, template), None);
}

#[test]
#[cfg(unix)]
fn agent_failures_get_a_hint_when_the_agent_socket_exists() {
    let directory = SocketDirectory::new();
    let (socket, _listener) = directory.socket();
    let stderr = "Could not open a connection to your authentication agent.\n";
    assert_eq!(context().hint(&output(2, stderr)), None);
    let with_agent = HintContext {
        workspace_roots: vec![PathBuf::from(WORKSPACE)],
        agent_socket: Some(socket),
    };
    assert_eq!(
        with_agent.hint(&output(2, stderr)),
        Some(SandboxHint::HostSocket)
    );
}
