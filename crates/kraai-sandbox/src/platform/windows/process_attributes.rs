use std::io;

use windows_sys::Win32::System::Threading::{
    DeleteProcThreadAttributeList, InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
    UpdateProcThreadAttribute,
};

pub(super) struct Attributes {
    storage: Vec<usize>,
}

impl Attributes {
    #[expect(
        unsafe_code,
        reason = "Windows allocates process attribute lists into an aligned caller-owned buffer"
    )]
    pub(super) fn new(count: u32) -> io::Result<Self> {
        let mut size = 0;
        unsafe { InitializeProcThreadAttributeList(std::ptr::null_mut(), count, 0, &mut size) };
        if size == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut storage = vec![0_usize; size.div_ceil(std::mem::size_of::<usize>())];
        if unsafe {
            InitializeProcThreadAttributeList(storage.as_mut_ptr().cast(), count, 0, &mut size)
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { storage })
    }

    pub(super) fn as_ptr(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.storage.as_mut_ptr().cast()
    }

    pub(super) fn add<T>(&mut self, key: u32, value: &T) -> io::Result<()> {
        self.add_slice(key, std::slice::from_ref(value))
    }

    #[expect(
        unsafe_code,
        reason = "attribute values remain alive until process creation and attribute-list deletion"
    )]
    pub(super) fn add_slice<T>(&mut self, key: u32, value: &[T]) -> io::Result<()> {
        if unsafe {
            UpdateProcThreadAttribute(
                self.as_ptr(),
                0,
                key as usize,
                value.as_ptr().cast(),
                std::mem::size_of_val(value),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

impl Drop for Attributes {
    #[expect(
        unsafe_code,
        reason = "the initialized Windows attribute list must be destroyed before freeing its storage"
    )]
    fn drop(&mut self) {
        unsafe { DeleteProcThreadAttributeList(self.as_ptr()) };
    }
}
