// Copyright (c) 2026 tre4surehunter9
//! The global allocator for user programs, so they can use `alloc`
//! (Vec, String, Box, BTreeMap, ...). Memory comes from brk(2): the heap
//! starts empty and grows in 64 KiB steps when an allocation doesn't fit.

use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::ptr::{null_mut, NonNull};
use linked_list_allocator::Heap;

const GROW_STEP: usize = 64 * 1024;

struct BrkAllocator {
    heap: UnsafeCell<Heap>,
}

// User programs are single-threaded, so there's no concurrent access.
unsafe impl Sync for BrkAllocator {}

#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator { heap: UnsafeCell::new(Heap::empty()) };

impl BrkAllocator {
    /// Grow the heap by at least `min` bytes. Returns false if out of memory.
    unsafe fn grow(&self, min: usize) -> bool {
        let heap = unsafe { &mut *self.heap.get() };
        let by = (min.max(GROW_STEP) + 4095) & !4095;
        if heap.size() == 0 {
            let start = crate::brk(0);
            if crate::brk(start + by as u64) != start + by as u64 {
                return false;
            }
            unsafe { heap.init(start as usize, by) };
        } else {
            let top = heap.top() as u64;
            if crate::brk(top + by as u64) != top + by as u64 {
                return false;
            }
            unsafe { heap.extend(by) };
        }
        true
    }
}

unsafe impl GlobalAlloc for BrkAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        loop {
            let heap = unsafe { &mut *self.heap.get() };
            if let Ok(ptr) = heap.allocate_first_fit(layout) {
                return ptr.as_ptr();
            }
            // Room for the block plus alignment slack and allocator bookkeeping.
            if !unsafe { self.grow(layout.size() + layout.align() + 64) } {
                return null_mut();
            }
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let heap = unsafe { &mut *self.heap.get() };
        if let Some(ptr) = NonNull::new(ptr) {
            unsafe { heap.deallocate(ptr, layout) };
        }
    }
}
