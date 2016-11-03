//! A heap allocator that gives ownership of the value like a `Box`, but allocates in batches.
//!
//! `Pool` allocates objects on the heap in batches.
//! All objects must be of the same type like in `Vec`.
//! It allows fast multithreaded parallel allocation of objects.
//! If the `Pool` runs out of memory, it will allocate more but will not move the old values
//! from their place.
//!
//! When objects are dropped, their memory is returned to the pool to be reused for
//! future allocations.
//! Only when all the objects and the `Pool` are dropped will the memory be released.
//!
//! It gives the option to allocate each object on a separate CPU cache line, increasing performance
//! of multithreaded access to adjacent elements.
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
//! let pool = Pool::new(1000, true);
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
//! let memory_pool = veryfast::pool::Pool::new(1000, true);
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

use alloc::heap;
use std::fmt;
use std::mem;
use std::ops::{Deref, DerefMut};
use std::ptr;
use std::sync::{Arc, Mutex};

use super::crossbeam::sync::MsQueue;

/// A fast heap-allocator. Allocates objects in a batch, but transfers the control to the `Object`.
///
/// Allocations will first check if there is an already free slot to use, and use that.
/// If no, It will take a lock and allocate a batch of memory.
///
/// When objects are dropped, their memory will be returned to the pool to be used again later.
/// The memory of the batches will be deallocated only when the `Pool` and all the related `Object`s
/// are dropped.
pub struct Pool<T> {
    manager: Arc<Manager<T>>,
}

/// A pointer type that owns its content.
///
/// Created from a `Pool`. The `Object` owns the value inside it and has exclusive access to it.
///
pub struct Object<T> {
    obj: *mut T,
    manager: Arc<Manager<T>>,
}

struct Manager<T> {
    data: Mutex<Vec<*const T>>,
    free: MsQueue<*mut T>,
    batch: usize,
    align: usize,
    memory_size: usize,
    elem_size: usize,
}

impl<T> Pool<T> {
    /// Creates a new `Pool`.
    ///
    /// - `batch`: How many objects should be allocated each time. Higher numbers are faster,
    /// but can cause wasted memory if too little are actually used.
    ///
    /// - `align_to_cache`: Should each object be on a separate CPU cache line. Speeds up
    /// multithreaded usage but requires a bit more memory in most cases.
    #[inline]
    pub fn new(batch: usize, align_to_cache: bool) -> Pool<T> {
        assert!(batch != 0, "Pool requested with batch = 0");
        assert!(mem::size_of::<T>() != 0,
                "Pool requested with type of size 0");
        Pool { manager: Arc::new(Manager::new(align_to_cache, batch)) }
    }

    /// Save the object on the heap. Will get a pointer that will drop it's content when
    /// dropped (like a `Box`). The memory will be reused though!
    ///
    /// Thread-safe. Very fast most of the time, but will take a bit longer if need to allocate
    /// more objects.
    ///
    /// Will panic if out of memory.
    #[inline]
    pub fn insert(&self, obj: T) -> Object<T> {
        self.manager.insert(obj, self.manager.clone())
    }
}

impl<T> Manager<T> {
    #[inline]
    pub fn new(align_to_cache: bool, batch: usize) -> Manager<T> {
        let mut align = mem::align_of::<T>();
        let mut elem_size = mem::size_of::<T>();
        if align_to_cache {
            let cache_line_size = 64;
            align = ((align - 1) / cache_line_size + 1) * cache_line_size;
            elem_size = ((elem_size - 1) / cache_line_size + 1) * cache_line_size;
        }
        let memory_size = elem_size * batch;
        Manager::<T> {
            data: Mutex::new(Vec::new()),
            free: MsQueue::new(),
            batch: batch,
            align: align,
            memory_size: memory_size,
            elem_size: elem_size,
        }
    }

    #[inline]
    fn expand(&self) -> *mut T {
        unsafe {
            let mut lock = self.data.lock().unwrap();
            if let Some(x) = self.free.try_pop() {
                return x;
            }
            let extra = heap::allocate(self.memory_size, self.align) as *mut T;
            if extra.is_null() {
                panic!("out of memory");
            }
            // starting from 1 since index 0 will be returned
            for i in 1..self.batch {
                self.free.push((extra as usize + i * self.elem_size) as *mut T);
            }
            lock.push(extra);
            extra
        }
    }

    #[inline]
    pub fn insert(&self, obj: T, manager: Arc<Manager<T>>) -> Object<T> {
        let slot = match self.free.try_pop() {
            Some(x) => x,
            None => self.expand(),
        };
        unsafe {
            ptr::write(slot, obj);
        }
        Object {
            obj: slot,
            manager: manager,
        }
    }

    #[inline]
    pub fn ret_ptr(&self, obj: *mut T) {
        self.free.push(obj);
    }
}

impl<T> Drop for Manager<T> {
    #[inline]
    fn drop(&mut self) {
        let lock = match self.data.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        for block in lock.deref() {
            unsafe {
                heap::deallocate(*block as *mut u8, self.memory_size, self.align);
            }
        }
    }
}

impl<T> Object<T> {
    /// Returns the owned object from the pool-allocated memory.
    #[inline]
    pub fn recover(self) -> T {
        let ret = unsafe {
            ptr::read(self.obj)
        };
        self.manager.ret_ptr(self.obj);
        ret
    }
}

impl<T> Drop for Object<T> {
    #[inline]
    fn drop(&mut self) {
        unsafe {
            ptr::read(self.obj);
        }
        (*self.manager).ret_ptr(self.obj);
    }
}

impl<T> Deref for Object<T> {
    type Target = T;

    #[allow(inline_always)]
    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        unsafe { &*self.obj }
    }
}

impl<T> DerefMut for Object<T> {
    #[allow(inline_always)]
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.obj }
    }
}

unsafe impl<T: Send> Send for Object<T> {}

unsafe impl<T: Sync> Sync for Object<T> {}

unsafe impl<T: Send> Send for Manager<T> {}

unsafe impl<T: Send> Sync for Manager<T> {}

impl<T> fmt::Debug for Pool<T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f,
               "{} objects in {:?}",
               Arc::strong_count(&self.manager) - 1,
               self.manager)
    }
}

impl<T> fmt::Debug for Manager<T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let len = {
            self.data.lock().unwrap().len()
        };
        write!(f,
               "{} blocks, {} bytes allocated for {} possible elements",
               len,
               self.memory_size * len,
               self.batch * len)
    }
}

// impl<T> fmt::Debug for Object<T>
// where T: fmt::Debug
// {
// #[inline]
// fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
// (**self).fmt(f)
// }
// }
//
// impl<T> fmt::Display for Object<T>
// where T: fmt::Display
// {
// #[inline]
// fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
// (**self).fmt(f)
// }
// }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_dereference() {
        let val = 5u64;
        let pool = Pool::new(10, false);
        let mut val2 = pool.insert(val);
        assert_eq!(*val2, val);
        let val3 = 7u64;
        *val2 = val3;
        assert_eq!(*val2, val3);
    }
}
