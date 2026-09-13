#![cfg(windows)]
#![expect(
    unsafe_code,
    reason = "native experiment verifies token-scoped Windows network filtering"
)]
#![expect(clippy::expect_used, reason = "native test setup must succeed")]

use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr;
use std::time::Duration;

use windows_sys::Win32::Foundation::{HANDLE, LocalFree, WAIT_OBJECT_0};
use windows_sys::Win32::NetworkManagement::WindowsFilteringPlatform::*;
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, ConvertStringSidToSidW,
};
use windows_sys::Win32::Security::*;
use windows_sys::Win32::System::Rpc::RPC_C_AUTHN_WINNT;
use windows_sys::Win32::System::Threading::*;

struct Engine(HANDLE);

impl Drop for Engine {
    fn drop(&mut self) {
        unsafe { FwpmEngineClose0(self.0) };
    }
}

struct Allocation(*mut std::ffi::c_void);

impl Drop for Allocation {
    fn drop(&mut self) {
        unsafe { LocalFree(self.0) };
    }
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain([0]).collect()
}

#[test]
fn network_probe() {
    let Ok(address) = std::env::var("KRAAI_WFP_ADDRESS") else {
        return;
    };
    let address = address.parse().expect("parse address");
    let allowed = std::env::var("KRAAI_WFP_ALLOWED").expect("expected permission") == "1";
    assert_eq!(
        std::net::TcpStream::connect_timeout(&address, Duration::from_secs(2)).is_ok(),
        allowed
    );
}

#[test]
fn restricted_identity_controls_network_without_affecting_host() {
    let session = FWPM_SESSION0 {
        flags: FWPM_SESSION_FLAG_DYNAMIC,
        ..Default::default()
    };
    let mut engine = ptr::null_mut();
    assert_eq!(
        unsafe {
            FwpmEngineOpen0(
                ptr::null(),
                RPC_C_AUTHN_WINNT,
                ptr::null(),
                &session,
                &mut engine,
            )
        },
        0,
        "open filtering engine as administrator"
    );
    let engine = Engine(engine);
    let marker_name = format!(
        "S-1-5-21-{}-{}-{}-1000",
        rand::random::<u32>(),
        rand::random::<u32>(),
        rand::random::<u32>()
    );
    let mut marker = ptr::null_mut();
    assert_ne!(
        unsafe { ConvertStringSidToSidW(wide(&marker_name).as_ptr(), &mut marker) },
        0
    );
    let marker = Allocation(marker);
    let sddl = wide(&format!("D:(A;;CC;;;{marker_name})"));
    let mut descriptor = ptr::null_mut();
    let mut size = 0;
    assert_ne!(
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                1,
                &mut descriptor,
                &mut size,
            )
        },
        0
    );
    let descriptor = Allocation(descriptor);
    let mut blob = FWP_BYTE_BLOB {
        size,
        data: descriptor.0.cast(),
    };
    let mut condition = FWPM_FILTER_CONDITION0 {
        fieldKey: FWPM_CONDITION_ALE_USER_ID,
        matchType: FWP_MATCH_EQUAL,
        conditionValue: FWP_CONDITION_VALUE0 {
            r#type: FWP_SECURITY_DESCRIPTOR_TYPE,
            Anonymous: FWP_CONDITION_VALUE0_0 { sd: &mut blob },
        },
    };
    for layer in [
        FWPM_LAYER_ALE_AUTH_CONNECT_V4,
        FWPM_LAYER_ALE_AUTH_CONNECT_V6,
    ] {
        let mut filter = FWPM_FILTER0 {
            layerKey: layer,
            subLayerKey: FWPM_SUBLAYER_UNIVERSAL,
            numFilterConditions: 1,
            filterCondition: &mut condition,
            action: FWPM_ACTION0 {
                r#type: FWP_ACTION_BLOCK,
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(
            unsafe { FwpmFilterAdd0(engine.0, &mut filter, ptr::null(), ptr::null_mut()) },
            0,
            "install token filter"
        );
    }
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("host listener");
    let address = listener.local_addr().expect("listener address");
    assert!(
        std::net::TcpStream::connect_timeout(&address, Duration::from_secs(2)).is_ok(),
        "host unaffected"
    );
    let mut base = ptr::null_mut();
    assert_ne!(
        unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_ALL_ACCESS, &mut base) },
        0
    );
    let base = unsafe { OwnedHandle::from_raw_handle(base) };
    let mut user = vec![0_u64; 512];
    let mut length = 0;
    assert_ne!(
        unsafe {
            GetTokenInformation(
                base.as_raw_handle(),
                TokenUser,
                user.as_mut_ptr().cast(),
                (user.len() * 8) as u32,
                &mut length,
            )
        },
        0
    );
    let user_sid = unsafe { (*user.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    for offline in [false, true] {
        let mut sids = vec![SID_AND_ATTRIBUTES {
            Sid: user_sid,
            Attributes: 0,
        }];
        if offline {
            sids.push(SID_AND_ATTRIBUTES {
                Sid: marker.0,
                Attributes: 0,
            });
        }
        let mut token = ptr::null_mut();
        assert_ne!(
            unsafe {
                CreateRestrictedToken(
                    base.as_raw_handle(),
                    DISABLE_MAX_PRIVILEGE | WRITE_RESTRICTED,
                    0,
                    ptr::null(),
                    0,
                    ptr::null(),
                    sids.len() as u32,
                    sids.as_ptr(),
                    &mut token,
                )
            },
            0
        );
        let token = unsafe { OwnedHandle::from_raw_handle(token) };
        run_probe(&token, &address.to_string(), !offline);
    }
}

fn run_probe(token: &OwnedHandle, address: &str, allowed: bool) {
    let executable = std::env::current_exe().expect("test executable");
    let application: Vec<u16> = executable.as_os_str().encode_wide().chain([0]).collect();
    let mut command = wide(&format!(
        "\"{}\" --exact network_probe --nocapture",
        executable.display()
    ));
    let mut environment = std::env::vars_os().collect::<std::collections::BTreeMap<_, _>>();
    environment.insert("KRAAI_WFP_ADDRESS".into(), address.into());
    environment.insert(
        "KRAAI_WFP_ALLOWED".into(),
        if allowed { "1" } else { "0" }.into(),
    );
    let mut block = Vec::new();
    for (name, value) in environment {
        block.extend(name.encode_wide());
        block.push(b'=' as u16);
        block.extend(value.encode_wide());
        block.push(0);
    }
    block.push(0);
    let startup = STARTUPINFOW {
        cb: size_of::<STARTUPINFOW>() as u32,
        ..Default::default()
    };
    let mut information = PROCESS_INFORMATION::default();
    assert_ne!(
        unsafe {
            CreateProcessAsUserW(
                token.as_raw_handle(),
                application.as_ptr(),
                command.as_mut_ptr(),
                ptr::null(),
                ptr::null(),
                0,
                CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT,
                block.as_ptr().cast(),
                ptr::null(),
                &startup,
                &mut information,
            )
        },
        0,
        "launch restricted probe: {}",
        std::io::Error::last_os_error()
    );
    let process = unsafe { OwnedHandle::from_raw_handle(information.hProcess) };
    let _thread = unsafe { OwnedHandle::from_raw_handle(information.hThread) };
    let wait = unsafe { WaitForSingleObject(process.as_raw_handle(), 10000) };
    if wait != WAIT_OBJECT_0 {
        unsafe { TerminateProcess(process.as_raw_handle(), 1) };
    }
    assert_eq!(wait, WAIT_OBJECT_0, "probe completed");
    let mut code = 0;
    assert_ne!(
        unsafe { GetExitCodeProcess(process.as_raw_handle(), &mut code) },
        0
    );
    assert_eq!(code, 0, "network permission: {allowed}");
}
