use std::fs::File;
use std::io::{Seek, SeekFrom, Write};

use crate::error::SandboxError;

pub(super) fn restricted_network_seccomp_filter(
    network_enabled: bool,
    private_ipc_connect_descriptors: &[std::os::fd::RawFd],
) -> Result<Option<File>, SandboxError> {
    if network_enabled {
        return Ok(None);
    }

    create_seccomp_filter_file(private_ipc_connect_descriptors).map(Some)
}

fn create_seccomp_filter_file(
    private_ipc_connect_descriptors: &[std::os::fd::RawFd],
) -> Result<File, SandboxError> {
    let mut file = create_temp_seccomp_file()?;
    write_seccomp_filter(&mut file, private_ipc_connect_descriptors)?;
    file.seek(SeekFrom::Start(0))
        .map_err(seccomp_file_error("rewind seccomp filter file"))?;
    Ok(file)
}

#[cfg(target_os = "linux")]
fn create_temp_seccomp_file() -> Result<File, SandboxError> {
    let base = std::env::temp_dir();
    for attempt in 0..16_u8 {
        let path = base.join(format!(
            "kraai-bwrap-seccomp-{}-{}-{attempt}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => {
                let _ = std::fs::remove_file(path);
                return Ok(file);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(SandboxError::SandboxUnavailable(format!(
                    "unable to create seccomp filter file: {error}"
                )));
            }
        }
    }

    Err(SandboxError::SandboxUnavailable(String::from(
        "unable to create unique seccomp filter file",
    )))
}

#[cfg(target_os = "linux")]
fn seccomp_file_error(
    action: &'static str,
) -> impl FnOnce(std::io::Error) -> SandboxError + 'static {
    move |error| SandboxError::SandboxUnavailable(format!("unable to {action}: {error}"))
}

#[cfg(target_os = "linux")]
fn write_seccomp_filter(
    file: &mut File,
    private_ipc_connect_descriptors: &[std::os::fd::RawFd],
) -> Result<(), SandboxError> {
    for instruction in restricted_network_seccomp_program(private_ipc_connect_descriptors)? {
        file.write_all(&instruction.code.to_ne_bytes())
            .map_err(seccomp_file_error("write seccomp filter instruction"))?;
        file.write_all(&[instruction.jt, instruction.jf])
            .map_err(seccomp_file_error("write seccomp filter instruction"))?;
        file.write_all(&instruction.k.to_ne_bytes())
            .map_err(seccomp_file_error("write seccomp filter instruction"))?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy)]
pub(crate) struct SeccompInstruction {
    pub(crate) code: u16,
    pub(crate) jt: u8,
    pub(crate) jf: u8,
    pub(crate) k: u32,
}

#[cfg(target_os = "linux")]
fn stmt(code: u16, k: u32) -> SeccompInstruction {
    SeccompInstruction {
        code,
        jt: 0,
        jf: 0,
        k,
    }
}

#[cfg(target_os = "linux")]
fn jump(code: u16, k: u32, jt: u8, jf: u8) -> SeccompInstruction {
    SeccompInstruction { code, jt, jf, k }
}

#[cfg(target_os = "linux")]
pub(crate) fn restricted_network_seccomp_program(
    private_ipc_connect_descriptors: &[std::os::fd::RawFd],
) -> Result<Vec<SeccompInstruction>, SandboxError> {
    const BPF_LD_W_ABS: u16 = 0x20;
    const BPF_JMP_JEQ_K: u16 = 0x15;
    const BPF_RET_K: u16 = 0x06;
    const SECCOMP_DATA_NR_OFFSET: u32 = 0;
    const SECCOMP_DATA_ARCH_OFFSET: u32 = 4;
    const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
    const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;
    const DENY: u32 = SECCOMP_RET_ERRNO | libc::EPERM as u32;

    let Some(audit_arch) = audit_arch() else {
        return Err(SandboxError::SandboxUnavailable(String::from(
            "restricted-network seccomp is unsupported on this CPU architecture",
        )));
    };

    let mut program = vec![
        stmt(BPF_LD_W_ABS, SECCOMP_DATA_ARCH_OFFSET),
        jump(BPF_JMP_JEQ_K, audit_arch, 1, 0),
        stmt(BPF_RET_K, DENY),
        stmt(BPF_LD_W_ABS, SECCOMP_DATA_NR_OFFSET),
    ];
    append_arch_syscall_rejections(&mut program);

    append_af_unix_only_socket_rule(&mut program, libc::SYS_socket as u32);
    append_af_unix_only_socket_rule(&mut program, libc::SYS_socketpair as u32);
    append_private_ipc_connect_rule(&mut program, private_ipc_connect_descriptors)?;

    for syscall in [
        libc::SYS_ptrace,
        libc::SYS_process_vm_readv,
        libc::SYS_process_vm_writev,
        libc::SYS_io_uring_setup,
        libc::SYS_io_uring_enter,
        libc::SYS_io_uring_register,
        libc::SYS_accept,
        libc::SYS_accept4,
        libc::SYS_bind,
        libc::SYS_listen,
        libc::SYS_getpeername,
        libc::SYS_getsockname,
        libc::SYS_shutdown,
        libc::SYS_sendto,
        libc::SYS_sendmsg,
        libc::SYS_sendmmsg,
        libc::SYS_recvfrom,
        libc::SYS_recvmsg,
        libc::SYS_recvmmsg,
        libc::SYS_getsockopt,
        libc::SYS_setsockopt,
    ] {
        append_deny_syscall_rule(&mut program, syscall as u32);
    }

    program.push(stmt(BPF_RET_K, SECCOMP_RET_ALLOW));
    Ok(program)
}

#[cfg(target_os = "linux")]
fn append_private_ipc_connect_rule(
    program: &mut Vec<SeccompInstruction>,
    descriptors: &[std::os::fd::RawFd],
) -> Result<(), SandboxError> {
    if descriptors.is_empty() {
        append_deny_syscall_rule(program, libc::SYS_connect as u32);
        return Ok(());
    }

    let skip = u8::try_from(descriptors.len() + 3).map_err(|_error| {
        SandboxError::SandboxUnavailable(String::from(
            "too many private IPC descriptors for the seccomp program",
        ))
    })?;
    const BPF_LD_W_ABS: u16 = 0x20;
    const BPF_JMP_JEQ_K: u16 = 0x15;
    const BPF_RET_K: u16 = 0x06;
    const SECCOMP_DATA_ARG0_OFFSET: u32 = 16;
    const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
    const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;
    const DENY: u32 = SECCOMP_RET_ERRNO | libc::EPERM as u32;

    program.push(jump(BPF_JMP_JEQ_K, libc::SYS_connect as u32, 0, skip));
    program.push(stmt(BPF_LD_W_ABS, SECCOMP_DATA_ARG0_OFFSET));
    for (index, descriptor) in descriptors.iter().enumerate() {
        let remaining = descriptors.len() - index;
        let jump_to_allow = u8::try_from(remaining).map_err(|_error| {
            SandboxError::SandboxUnavailable(String::from(
                "too many private IPC descriptors for the seccomp program",
            ))
        })?;
        let descriptor = u32::try_from(*descriptor).map_err(|_error| {
            SandboxError::SandboxUnavailable(format!(
                "private IPC descriptor {descriptor} is invalid"
            ))
        })?;
        program.push(jump(BPF_JMP_JEQ_K, descriptor, jump_to_allow, 0));
    }
    program.push(stmt(BPF_RET_K, DENY));
    program.push(stmt(BPF_RET_K, SECCOMP_RET_ALLOW));
    Ok(())
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn append_arch_syscall_rejections(program: &mut Vec<SeccompInstruction>) {
    const BPF_JMP_JGE_K: u16 = 0x35;
    const BPF_RET_K: u16 = 0x06;
    const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;
    const X32_SYSCALL_BIT: u32 = 0x4000_0000;
    const DENY: u32 = SECCOMP_RET_ERRNO | libc::EPERM as u32;

    program.push(jump(BPF_JMP_JGE_K, X32_SYSCALL_BIT, 0, 1));
    program.push(stmt(BPF_RET_K, DENY));
}

#[cfg(all(target_os = "linux", not(target_arch = "x86_64")))]
fn append_arch_syscall_rejections(_program: &mut Vec<SeccompInstruction>) {}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn audit_arch() -> Option<u32> {
    Some(0xc000_003e)
}

#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
fn audit_arch() -> Option<u32> {
    Some(0xc000_00b7)
}

#[cfg(all(
    target_os = "linux",
    not(any(target_arch = "x86_64", target_arch = "aarch64"))
))]
fn audit_arch() -> Option<u32> {
    None
}

#[cfg(target_os = "linux")]
fn append_af_unix_only_socket_rule(program: &mut Vec<SeccompInstruction>, syscall: u32) {
    const BPF_LD_W_ABS: u16 = 0x20;
    const BPF_JMP_JEQ_K: u16 = 0x15;
    const BPF_RET_K: u16 = 0x06;
    const SECCOMP_DATA_ARG0_OFFSET: u32 = 16;
    const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
    const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;
    const DENY: u32 = SECCOMP_RET_ERRNO | libc::EPERM as u32;

    program.push(jump(BPF_JMP_JEQ_K, syscall, 0, 4));
    program.push(stmt(BPF_LD_W_ABS, SECCOMP_DATA_ARG0_OFFSET));
    program.push(jump(BPF_JMP_JEQ_K, libc::AF_UNIX as u32, 1, 0));
    program.push(stmt(BPF_RET_K, DENY));
    program.push(stmt(BPF_RET_K, SECCOMP_RET_ALLOW));
}

#[cfg(target_os = "linux")]
fn append_deny_syscall_rule(program: &mut Vec<SeccompInstruction>, syscall: u32) {
    const BPF_JMP_JEQ_K: u16 = 0x15;
    const BPF_RET_K: u16 = 0x06;
    const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;
    const DENY: u32 = SECCOMP_RET_ERRNO | libc::EPERM as u32;

    program.push(jump(BPF_JMP_JEQ_K, syscall, 0, 1));
    program.push(stmt(BPF_RET_K, DENY));
}
