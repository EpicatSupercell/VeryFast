//! A queue-like buffer, which allows asynchronous insertions. Reading from the queue requires
//! exclusive access.
//!
//! The memory is allocated in a `Pool`, and distributed as needed.

use std::mem::uninitialized;
use std::ptr::null_mut;
use std::ptr::read;
use std::ptr::write;
use std::sync::atomic::AtomicPtr;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering::Acquire;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::atomic::Ordering::Release;
use std::sync::Mutex;

use pool::Pool;
use pool::Object;

const BUFFER_SIZE: usize = 16;

pub struct TinyBufferPool<'p, T: 'p> {
    pool: Pool<TinyLinkedBuffer<'p, T>>,
}

pub struct TinyBuffer<'p, T: 'p> {
    buf: TinyLinkedPointer<'p, T>,
    len: AtomicUsize,
    pool: &'p Pool<TinyLinkedBuffer<'p, T>>,
}

struct TinyLinkedPointer<'p, T: 'p> {
    ptr: AtomicPtr<TinyLinkedBuffer<'p, T>>,
    alloc: Mutex<Option<Object<'p, TinyLinkedBuffer<'p, T>>>>,
}

struct TinyLinkedBuffer<'p, T: 'p> {
    data: [T; BUFFER_SIZE],
    next: TinyLinkedPointer<'p, T>,
}

pub struct IterMut<'i, 'p: 'i, T: 'p> {
    next: usize,
    left: usize,
    buf: Option<&'i TinyLinkedBuffer<'p, T>>,
}

pub struct Drain<'p, T: 'p> {
    next: usize,
    left: usize,
    buf: Option<Object<'p, TinyLinkedBuffer<'p, T>>>,
}

impl<'p, T> TinyBufferPool<'p, T> {
    pub fn new() -> Self {
        TinyBufferPool { pool: Pool::new() }
    }

    pub fn create(&'p self) -> TinyBuffer<'p, T> {
        TinyBuffer {
            buf: TinyLinkedPointer::empty(),
            len: AtomicUsize::new(0),
            pool: &self.pool,
        }
    }
}

impl<'p, T> TinyBuffer<'p, T> {
    pub fn push(&self, item: T) {
        let pos = self.len.fetch_add(1, Relaxed);
        unsafe {
            let slot = self.buf
                .get(&|item: TinyLinkedBuffer<'p, T>| self.pool.insert(item))
                .get(pos, &|item: TinyLinkedBuffer<'p, T>| self.pool.insert(item));
            write(slot, item);
        }
    }

    pub fn iter_mut<'i>(&'i mut self) -> IterMut<'i, 'p, T> {
        IterMut {
            next: 0,
            left: self.len.load(Relaxed),
            buf: self.buf.try_get(),
        }
    }

    pub fn drain(&mut self) -> Drain<'p, T> {
        Drain {
            next: 0,
            left: self.len.load(Relaxed),
            buf: self.buf.steal(),
        }
    }
}

impl<'p, T> TinyLinkedPointer<'p, T> {
    fn empty() -> Self {
        TinyLinkedPointer {
            ptr: AtomicPtr::new(null_mut()),
            alloc: Mutex::new(None),
        }
    }

    fn get<F>(&self, func: &F) -> &TinyLinkedBuffer<'p, T>
        where F: Fn(TinyLinkedBuffer<'p, T>) -> Object<'p, TinyLinkedBuffer<'p, T>>
    {
        let ptr = self.ptr.load(Relaxed);
        if ptr.is_null() {
            let mut lock = self.alloc.lock().unwrap();
            if lock.is_some() {
                let ptr = self.ptr.load(Acquire);
                return unsafe { &*ptr };
            };
            {
                *lock = Some(func(TinyLinkedBuffer::new()));
                if let Some(ref obj) = *lock {
                    let ptr = (&**obj) as *const _;
                    self.ptr.store(ptr as *mut _, Release);
                    unsafe { &*ptr }
                } else {
                    unreachable!()
                }
            }
        } else {
            unsafe { &*ptr }
        }
    }

    fn try_get<'r>(&'r self) -> Option<&'r TinyLinkedBuffer<'p, T>> {
        let ptr = self.ptr.load(Relaxed);
        if ptr.is_null() {
            None
        } else {
            Some(unsafe { &*ptr })
        }
    }

    fn steal(&mut self) -> Option<Object<'p, TinyLinkedBuffer<'p, T>>> {
        self.ptr.store(null_mut(), Release);
        self.alloc.get_mut().unwrap().take()
    }
}

impl<'p, T> TinyLinkedBuffer<'p, T> {
    fn new() -> Self {
        TinyLinkedBuffer {
            data: unsafe { uninitialized() },
            next: TinyLinkedPointer::empty(),
        }
    }

    unsafe fn get<F>(&self, pos: usize, func: &F) -> *mut T
        where F: Fn(TinyLinkedBuffer<'p, T>) -> Object<'p, TinyLinkedBuffer<'p, T>>
    {
        if pos < BUFFER_SIZE {
            &self.data[pos] as *const _ as *mut _
        } else {
            self.next.get(func).get(pos - BUFFER_SIZE, func)
        }
    }
}

impl<'p, T> Drop for TinyBuffer<'p, T> {
    fn drop(&mut self) {
        self.drain();
    }
}

impl<'i, 'p, T> Iterator for IterMut<'i, 'p, T> {
    type Item = &'i mut T;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if self.next >= self.left {
                return None;
            }
            let next_buf = match self.buf {
                Some(buf) => {
                    if self.next < BUFFER_SIZE {
                        let next = self.next;
                        self.next += 1;
                        return Some(unsafe { &mut *((&buf.data[next]) as *const _ as *mut _) });
                    }
                    buf.next.try_get()
                }
                None => return None,
            };
            self.next -= BUFFER_SIZE;
            self.left -= BUFFER_SIZE;
            self.buf = next_buf;
        }
    }
}

impl<'p, T> Iterator for Drain<'p, T> {
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if self.next >= self.left {
                return None;
            }
            let next_buf = match self.buf {
                Some(ref mut buf) => {
                    if self.next < BUFFER_SIZE {
                        let next = self.next;
                        self.next += 1;
                        return Some(unsafe { read((&buf.data[next]) as *const _ as *mut _) });
                    }
                    buf.next.steal()
                }
                None => return None,
            };
            self.next -= BUFFER_SIZE;
            self.left -= BUFFER_SIZE;
            self.buf = next_buf;
        }
    }
}

impl<'p, T> Drop for Drain<'p, T> {
    fn drop(&mut self) {
        for _ in self {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::TinyBufferPool;
    use crossbeam::scope;

    #[test]
    fn creation_burrow_rules() {
        let pool = TinyBufferPool::<i32>::new();
        {
            pool.create();
        }
    }

    #[test]
    fn multiple_insertion_loops() {
        let pool = TinyBufferPool::<i32>::new();
        {
            let mut buf = pool.create();
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
}