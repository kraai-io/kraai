use std::io;
use std::os::windows::io::{AsRawHandle, IntoRawHandle};
use std::path::Path;

use windows_sys::Win32::Foundation::{
    GetHandleInformation, HANDLE_FLAG_INHERIT, SetHandleInformation,
};

use super::{Listener, connect};

#[tokio::test]
#[expect(
    unsafe_code,
    reason = "the test inspects the inheritance flags on a live owned pipe handle"
)]
async fn claiming_transport_prevents_inheritance_by_external_commands() {
    let mut listener = Listener::bind(Path::new(".")).unwrap();
    let client = listener.client.take().expect("missing client handle");
    // SAFETY: The test owns this live handle for the duration of the call.
    if unsafe {
        SetHandleInformation(
            client.as_raw_handle(),
            HANDLE_FLAG_INHERIT,
            HANDLE_FLAG_INHERIT,
        )
    } == 0
    {
        panic!("{}", io::Error::last_os_error());
    }
    let endpoint = (client.into_raw_handle() as usize).to_string();
    let transport = connect(Path::new(&endpoint)).unwrap();
    let mut flags = 0;
    // SAFETY: The file owns a live pipe handle and flags is a valid output pointer.
    if unsafe { GetHandleInformation(transport.as_raw_handle(), &mut flags) } == 0 {
        panic!("{}", io::Error::last_os_error());
    }
    if flags & HANDLE_FLAG_INHERIT != 0 {
        panic!("transport handle remains inheritable");
    }
    listener.server.connect().await.unwrap();
}
