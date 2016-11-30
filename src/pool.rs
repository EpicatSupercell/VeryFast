//! A heap allocator that gives ownership of the value like a `Box`, but allocates in batches.
//! It does not move already-allocated objects like a `Vec`, but does increase it's capacity
//! when needed.
//!
//! `Pool` allocates objects on the heap in batches.
//! All objects must be of the same type like in `Vec`.
//! It allows fast multithreaded parallel allocation of objects.
//! If the `Pool` runs out of memory, it will allocate more but will not move the old values
//! from their place.
//!
//! When objects are dropped, their memory is returned to the pool to be reused for
//! future allocations.
//! Objects are not able to outlive the pool.
//!
//! The `Object` class has exclusive ownership of the value contained within. When dropped, the
//! owned object will be dropped as well. The memory, however, will be returned to the `Pool` it
//! was allocated from to be available for other allocations.
//!
//! # Examples
//!
//! ```
//! use veryfast::pool::{Pool, Object};
//!
//! let pool = Pool::new();
//!
//! let var1 = pool.insert(15i32);
//! let mut var2 = pool.insert(7);
//! *var2 = *var1;
//! assert_eq!(*var1, *var2);
//!
//! let mut vec = Vec::new();
//! for i in 0..10 {
//!     vec.push(pool.insert(i));
//! }
//! for i in &vec {
//!     print!("{} ", **i);
//! }
//! ```
//!
//! An example using a scoped thread pool:
//!
//! ```
//! # extern crate veryfast;
//! extern crate scoped_threadpool;
//!
//! fn slow(val: &mut i32) {
//!     *val += 1;
//! }
//!
//! let mut thread_pool = scoped_threadpool::Pool::new(4);
//! let memory_pool = veryfast::pool::Pool::new();
//!
//! let mut vec = Vec::new();
//!
//! for i in 0..10 {
//!     vec.push(memory_pool.insert(i));
//! }
//!
//! thread_pool.scoped(|scoped| {
//!     for e in &mut vec {
//!         scoped.execute(move || {
//!             slow(&mut **e);
//!         });
//!     }
//! });
//!
//! for i in 0..10 {
//!     assert_eq!(*vec[i], vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10][i]);
//! }
//! ```
use std::fmt::Debug;
use std::fmt::Formatter;
use std::fmt::Result as FmtResult;
use std::ops::Deref;
use std::ops::DerefMut;
use std::ptr::read;
use std::ptr::write;
use std::sync::Mutex;

use crossbeam::sync::TreiberStack;

const BATCH_SIZE: usize = 64;

/// A fast heap-allocator. Allocates objects in a batch, but transfers the control to the `Object`.
///
/// Allocations will first check if there is an already free slot to use, and use that.
/// If no, It will take a lock and allocate a batch of memory.
///
/// When objects are dropped, their memory will be returned to the pool to be used again later.
/// `Pool` uses lifetimes to guarantee that all objects are dropped before the `Pool` is dropped.
pub struct Pool<T> {
    data: Mutex<Vec<Vec<T>>>,
    free: TreiberStack<*mut T>,
}

/// A pointer type that owns its content.
///
/// Created from a `Pool`. The `Object` owns the value inside it and has exclusive access to it.
pub struct Object<'p, T: 'p> {
    obj: &'p mut T,
    pool: &'p Pool<T>,
}

impl<T> Pool<T> {
    /// Creates a new `Pool`.
    /// The pool allocates 64 objects at a time.
    /// Customisation of this value will be provided when associated constants are available.
    #[inline]
    pub fn new() -> Self {
        Pool {
            data: Mutex::new(Vec::new()),
            free: TreiberStack::new(),
        }
    }

    /// Save the object on the heap. Will get a pointer that will drop it's content when
    /// dropped (like a `Box`). The memory will be reused though!
    ///
    /// Thread-safe. Very fast most of the time, but will take a bit longer if need to allocate
    /// more objects.
    #[inline]
    pub fn insert(&self, obj: T) -> Object<T> {
        let slot = match self.free.try_pop() {
            Some(x) => x,
            None => {
                let mut lock = self.data.lock().unwrap();
                if let Some(x) = self.free.try_pop() {
                    x
                } else {
                    let mut v = Vec::with_capacity(BATCH_SIZE);
                    let res = {
                        unsafe { v.set_len(BATCH_SIZE) };
                        let mut iter = v.iter_mut();
                        let res = iter.next().unwrap() as *mut _;
                        for i in iter.rev() {
                            self.free.push(i as *mut _);
                        }
                        res
                    };
                    lock.push(v);
                    res
                }
            }
        };
        unsafe {
            write(slot, obj);
            Object {
                obj: &mut *slot,
                pool: self,
            }
        }
    }

    #[inline]
    fn push_mem(&self, obj: *mut T) {
        self.free.push(obj);
    }
}

impl<'p, T> Object<'p, T> {
    /// Returns the owned object from the pool-allocated memory.
    #[inline]
    pub fn recover(obj: Self) -> T {
        let ret = unsafe { read(obj.obj) };
        obj.pool.push_mem(obj.obj);
        ret
    }

    /// Get access to the associated pool, for example to allocate data on the same pool.
    #[allow(inline_always)]
    #[inline(always)]
    pub fn pool<'a>(obj: &'a Self) -> &'p Pool<T> {
        obj.pool
    }
}

impl<'p, T> Deref for Object<'p, T> {
    type Target = T;
    #[allow(inline_always)]
    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        self.obj
    }
}

impl<'p, T> DerefMut for Object<'p, T> {
    #[allow(inline_always)]
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.obj
    }
}

impl<T> Drop for Pool<T> {
    #[inline]
    fn drop(&mut self) {
        let mut lock = self.data.lock().unwrap();
        for mut v in lock.drain(..) {
            unsafe { v.set_len(0) };
        }
    }
}

impl<'p, T> Drop for Object<'p, T> {
    #[inline]
    fn drop(&mut self) {
        unsafe { read(self.obj) };
        self.pool.push_mem(self.obj);
    }
}

impl<T> Debug for Pool<T> {
    #[inline]
    fn fmt(&self, fmt: &mut Formatter) -> FmtResult {
        write!(fmt,
               "Pool({} capacity)",
               BATCH_SIZE * self.data.lock().unwrap().len())
    }
}

impl<'p, T> Debug for Object<'p, T>
    where T: Debug
{
    #[inline]
    fn fmt(&self, fmt: &mut Formatter) -> FmtResult {
        write!(fmt, "Object {{ {:?} }}", &*self.obj)
    }
}

unsafe impl<T> Send for Pool<T> {}

unsafe impl<T> Sync for Pool<T> {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::RwLock;
    use std::thread::spawn;
    use crossbeam::scope;

    #[test]
    fn object_dereference() {
        let val = 5u64;
        let pool = Pool::new();
        let mut val2 = pool.insert(val);
        assert_eq!(*val2, val);
        let val3 = 7u64;
        *val2 = val3;
        assert_eq!(*val2, val3);
    }

    #[test]
    fn sync_send_attributes() {
        let pool = Pool::new();
        {
            let val2 = RwLock::new(pool.insert(5u64));
            scope(|s| {
                s.spawn(move || {
                    let mut val2 = val2.write().unwrap();
                    **val2 = 3 + **val2;
                })
            });
        }
        let x = RwLock::new(pool);
        spawn(move || {
                let a = x.read().unwrap();
                a.insert(3u64);
            })
            .join()
            .unwrap();
    }
}
