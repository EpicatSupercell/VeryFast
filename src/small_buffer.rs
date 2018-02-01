#![allow(mutex_atomic)]
//! `SmallBuffer` is useful as a buffer for elements that see little usage.
//! It has a small capacity inline, so a couple messages will not cause it to allocate memory.
//! If it receives more data than it can store, it will allocate additional memory to handle it.
//! It will not deallocate any memory, for cases when it's likely an element that has seen a lot of
//! usage has a higher chance to continue having high usage.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, AtomicPtr, Ordering};
use std::mem::uninitialized;
use std::ptr::{read, write, null_mut};

/// A small inline-allocated buffer with expansion capabilities. Pushing values can be done done asynchronously.
/// Reading values needs exclusive access. Removing values is only possible by draining the whole buffer.
///
/// The buffer is built like a linked list. Pushing many values at a time is discouraged. It fits well for cases where the
/// usual element count is low, but needs to be robust for the occasional peak.
///
/// Note: currently allocates 16 elements at a time. With `RFC #2000 - Const generics`
/// it will be possible to customize that number.
pub struct SmallBuffer<T> {
    buf: [T; 16],
    last_free_slot: AtomicUsize,
    next: AtomicPtr<SmallBuffer<T>>,
    unallocated_next: Mutex<bool>,
}

impl<T> SmallBuffer<T> {
    /// Creates an empty buffer with an initial capacity of 16.
    pub fn new() -> Self {
        let buf = unsafe { uninitialized() };
        SmallBuffer {
            buf: buf,
            last_free_slot: AtomicUsize::new(0),
            next: AtomicPtr::new(null_mut()),
            unallocated_next: Mutex::new(true),
        }
    }

    /// Pushes the item asynchronously, allocating more memory if needed.
    pub fn push(&self, item: T) {
        let index = self.last_free_slot.fetch_add(1, Ordering::AcqRel);
        self.insert_at_index(item, index);
    }

    fn insert_at_index(&self, item: T, index: usize) {
        if index < 16 {
            let slot = &self.buf[index] as *const T as *mut T;
            unsafe { write(slot, item) };
        } else {
            let index = index - 16;
            let next = self.next.load(Ordering::Acquire);
            unsafe {
                if !next.is_null() {
                    (*next).insert_at_index(item, index);
                } else {
                    let mut lock = self.unallocated_next.lock().unwrap();
                    if *lock {
                        *lock = false;
                        let b = Box::into_raw(Box::new(Self::new()));
                        self.next.store(b, Ordering::Release);
                        (*b).insert_at_index(item, index);
                    } else {
                        (*self.next.load(Ordering::Acquire)).insert_at_index(item, index);
                    }
                }
            }
        }
    }

    /// Creates a drain iterator. After the iterator is dropped, the buffer is empty.
    pub fn drain(&mut self) -> Drain<T> {
        let len = self.last_free_slot.load(Ordering::Relaxed);
        Drain {
            sb: self,
            next_index: 0,
            len: len,
        }
    }
}

impl<T> Drop for SmallBuffer<T> {
    fn drop(&mut self) {
        if self.last_free_slot.load(Ordering::Relaxed) != 0 {
            self.drain();
        }
        let next = self.next.load(Ordering::Relaxed);
        if !next.is_null() {
            unsafe { Box::from_raw(next) };
        }
    }
}

/// A draining iterator. Returns the contained elements one at a time, removing them from the
/// buffer. If the iterator is dropped, the remaining elements will be dropped and the buffer
/// returned to an empty state.
pub struct Drain<'a, T: 'a> {
    sb: &'a mut SmallBuffer<T>,
    next_index: usize,
    len: usize,
}

impl<'a, T> Iterator for Drain<'a, T> {
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next_index >= self.len {
            return None;
        }
        let val = {
            let slot = &mut self.sb.buf[self.next_index];
            unsafe { read(slot) }
        };
        self.next_index += 1;
        if self.next_index >= self.len {
            (*self.sb).last_free_slot.store(0, Ordering::Relaxed);
        } else if self.next_index >= 16 {
            (*self.sb).last_free_slot.store(0, Ordering::Relaxed);
            self.len -= 16;
            self.next_index -= 16;
            unsafe { self.sb = &mut *self.sb.next.load(Ordering::Relaxed) };
        }
        Some(val)
    }
}

impl<'a, T> Drop for Drain<'a, T> {
    fn drop(&mut self) {
        for _ in self {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam::scope;

    #[test]
    fn multiple_insertion_loops() {
        let mut buf = SmallBuffer::<i32>::new();
        {
            let buf_ref = &buf;
            scope(|s| {
                for i in 0..140 {
                    s.spawn(move || buf_ref.push(i));
                }
            });
        }
        let count = buf.drain().count();
        assert_eq!(count, 140);
        {
            let buf_ref = &buf;
            scope(|s| {
                for i in 0..70 {
                    s.spawn(move || buf_ref.push(i));
                }
            });
        }
        let count = buf.drain().count();
        assert_eq!(count, 70);
    }
}