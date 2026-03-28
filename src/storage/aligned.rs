use std::alloc::{Layout, alloc, dealloc};
use std::ops::{Deref, DerefMut};
use std::ptr::NonNull;

pub struct AlignedBuffer {
    ptr: NonNull<u8>,
    layout: Layout,
    len: usize,
}

impl AlignedBuffer {
    pub fn new(len: usize) -> Self {
        let align = 4096;
        let layout = Layout::from_size_align(len, align).expect("Invalid layout");

        let ptr = unsafe {
            let p = alloc(layout);
            if p.is_null() {
                std::alloc::handle_alloc_error(layout);
            }
            std::ptr::write_bytes(p, 0, len);
            NonNull::new_unchecked(p)
        };

        Self { ptr, layout, len }
    }

    pub fn as_ptr(&self) -> *mut u8 {
        self.ptr.as_ptr()
    }
}

impl Drop for AlignedBuffer {
    fn drop(&mut self) {
        unsafe {
            dealloc(self.ptr.as_ptr(), self.layout);
        }
    }
}

unsafe impl Send for AlignedBuffer {}
unsafe impl Sync for AlignedBuffer {}
