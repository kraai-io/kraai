use std::ffi::c_void;
use std::io;
use std::os::windows::io::{AsRawHandle, OwnedHandle};
use std::sync::Mutex;

use tokio::sync::oneshot;
use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Threading::{
    INFINITE, RegisterWaitForSingleObject, UnregisterWaitEx, WT_EXECUTEINWAITTHREAD,
    WT_EXECUTEONLYONCE,
};

struct Registration {
    handle: usize,
    notification: Option<Box<Mutex<Option<oneshot::Sender<()>>>>>,
}

impl Registration {
    #[expect(
        unsafe_code,
        reason = "a Windows wait callback signals the async task without blocking a runtime thread"
    )]
    fn new(process: &OwnedHandle, sender: oneshot::Sender<()>) -> io::Result<Self> {
        let notification = Box::new(Mutex::new(Some(sender)));
        let context = std::ptr::from_ref(notification.as_ref()).cast::<c_void>();
        let mut handle = std::ptr::null_mut();
        if unsafe {
            RegisterWaitForSingleObject(
                &mut handle,
                process.as_raw_handle(),
                Some(completed),
                context,
                INFINITE,
                WT_EXECUTEONLYONCE | WT_EXECUTEINWAITTHREAD,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            handle: handle as usize,
            notification: Some(notification),
        })
    }
}

impl Drop for Registration {
    #[expect(
        unsafe_code,
        reason = "unregister synchronously before freeing callback data, including cancelled waits"
    )]
    fn drop(&mut self) {
        if unsafe { UnregisterWaitEx(self.handle as HANDLE, INVALID_HANDLE_VALUE) } == 0
            && let Some(notification) = self.notification.take()
        {
            let _ = Box::leak(notification);
        }
    }
}

#[expect(
    unsafe_code,
    reason = "the callback context is owned by Registration until all callbacks have completed"
)]
unsafe extern "system" fn completed(context: *mut c_void, _timed_out: bool) {
    let notification = unsafe { &*context.cast::<Mutex<Option<oneshot::Sender<()>>>>() };
    let sender = notification
        .lock()
        .ok()
        .and_then(|mut sender| sender.take());
    if let Some(sender) = sender {
        let _ = sender.send(());
    }
}

pub(super) async fn wait(process: &OwnedHandle) -> io::Result<()> {
    let (sender, receiver) = oneshot::channel();
    let registration = Registration::new(process, sender)?;
    let result = receiver.await.map_err(io::Error::other);
    drop(registration);
    result
}
