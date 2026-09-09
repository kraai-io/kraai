use crate::{SeccompInstruction, restricted_network_seccomp_program};

/// Execute the classic BPF subset emitted by the policy, including branch skips.
/// Scanning adjacent constants confuses argument checks with syscall comparisons.
pub(super) fn denies_call(program: &[SeccompInstruction], syscall: u32, args: [u64; 6]) -> bool {
    let arch = match std::env::consts::ARCH {
        "x86_64" => 0xc000_003e,
        "aarch64" => 0xc000_00b7,
        arch => panic!("unsupported test architecture: {arch}"),
    };
    let mut accumulator = 0;
    let mut pc = 0;
    loop {
        let instruction = program.get(pc).expect("BPF program must return a decision");
        pc += 1;
        match instruction.code {
            0x20 => {
                accumulator = match instruction.k {
                    0 => syscall,
                    4 => arch,
                    offset @ 16..=60 if offset % 4 == 0 => {
                        let value = args[((offset - 16) / 8) as usize];
                        (value >> (((offset - 16) % 8) * 8)) as u32
                    }
                    offset => panic!("unsupported BPF load: {offset}"),
                };
            }
            0x54 => accumulator &= instruction.k,
            0x15 | 0x35 => {
                let matches = if instruction.code == 0x15 {
                    accumulator == instruction.k
                } else {
                    accumulator >= instruction.k
                };
                pc += usize::from(if matches {
                    instruction.jt
                } else {
                    instruction.jf
                });
            }
            0x06 => return instruction.k & 0xffff_0000 == 0x0005_0000,
            code => panic!("unsupported BPF instruction: {code}"),
        }
    }
}

#[test]
fn argument_branches_do_not_masquerade_as_syscall_denials() {
    let program = restricted_network_seccomp_program(&[]).expect("build policy");
    for syscall in [
        libc::SYS_read,
        libc::SYS_write,
        libc::SYS_sendmsg,
        libc::SYS_recvmsg,
    ] {
        assert!(!denies_call(&program, syscall as u32, [0; 6]));
    }
    assert!(denies_call(&program, libc::SYS_bind as u32, [0; 6]));
    assert!(denies_call(
        &program,
        libc::SYS_connect as u32,
        [20, 0, 0, 0, 0, 0]
    ));
    assert!(!denies_call(&program, libc::SYS_sendto as u32, [0; 6]));
    for address in [1, 1_u64 << 32] {
        assert!(denies_call(
            &program,
            libc::SYS_sendto as u32,
            [0, 0, 0, 0, address, 0]
        ));
    }
    assert!(denies_call(
        &program,
        libc::SYS_sendto as u32,
        [0, 0, 0, 0, 0, 1]
    ));
}
