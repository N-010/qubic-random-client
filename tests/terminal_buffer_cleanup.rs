use rtoolbox::safe_string::SafeString;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

struct Observer;
static TARGET: AtomicPtr<u8> = AtomicPtr::new(std::ptr::null_mut());
static REMAINING: AtomicUsize = AtomicUsize::new(usize::MAX);

// Observe only our synthetic allocation, before System releases it. The allocator
// callback neither allocates nor panics and never reads freed or uninitialized bytes.
unsafe impl GlobalAlloc for Observer {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if ptr == TARGET.load(Ordering::SeqCst) {
            // All 16 bytes were initialized before truncation, with no reallocation.
            let bytes = unsafe { std::slice::from_raw_parts(ptr, 16) };
            REMAINING.store(bytes.iter().filter(|&&b| b != 0).count(), Ordering::SeqCst);
            TARGET.store(std::ptr::null_mut(), Ordering::SeqCst);
        }
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Observer = Observer;

#[test]
fn dropped_terminal_buffer_wipes_edited_away_characters() {
    for retained in [16, 4, 0] {
        let mut input = SafeString::from_string("synthetic-prefix".to_owned());
        REMAINING.store(usize::MAX, Ordering::SeqCst);
        TARGET.store(input.as_ptr().cast_mut(), Ordering::SeqCst);
        input.truncate(retained);
        drop(input);
        assert_eq!(REMAINING.load(Ordering::SeqCst), 0, "retained={retained}");
    }

    let input = SafeString::from_string("synthetic-prefix".to_owned());
    assert_eq!(input.into_inner(), "synthetic-prefix");
}
