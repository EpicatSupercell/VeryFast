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
//! let pool = Pool::new();
//!
//! let var1 = pool.push(15i32);
//! let mut var2 = pool.push(7);
//! *var2 = *var1;
//! assert_eq!(*var1, *var2);
//!
//! let mut vec = Vec::new();
//! for i in 0..10 {
//!     vec.push(pool.push(i));
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
//! let memory_pool = veryfast::pool::Pool::with_params(true);
//!
//! let mut vec = Vec::new();
//!
//! for i in 0..10 {
//!     vec.push(memory_pool.push(i));
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

use std::heap::{Heap, Layout, Alloc};
use std::fmt;
use std::mem;
use std::ops::{Deref, DerefMut};
use std::ptr;
use std::sync::Mutex;

use super::crossbeam::sync::MsQueue;

/// A fast heap-allocator. Allocates objects in a batch, but transfers the ownership to the `Object`.
///
/// Allocations will first check if there is an already free slot to use, and use that.
/// If no, It will take a lock and allocate a batch of memory.
///
/// When objects are dropped, their memory will be returned to the pool to be used again later.
/// The memory of the batches will be deallocated only when the `Pool` and all the related `Object`s
/// are dropped.
pub struct Pool<T> {
    data: Mutex<Vec<*const T>>,
    free: MsQueue<*mut T>,
    layout: Layout,
    batch: usize,
    stride: usize,
}

/// A pointer type that owns its content.
///
/// Created from a `Pool`. The `Object` owns the value inside it and has exclusive access to it.
///
pub struct Object<'active, T: 'active> {
    obj: *mut T,
    manager: &'active Pool<T>,
}

impl<T> Pool<T> {
    /// Creates a new `Pool`.
    #[inline]
    pub fn new() -> Pool<T> {
        Pool::with_params(false)
    }

    /// Creates a new `Pool`.
    ///
    /// - `align_to_cache`: Should each object be on a separate CPU cache line. Speeds up
    /// multithreaded usage, but hurts single-threaded cache locality a bit and requires a bit more memory.
    /// Has no effect if `size_of::<T>` is already a multiple of a cache line size.
    #[inline]
    pub fn with_params(align_to_cache: bool) -> Pool<T> {
        Pool::with_system_params(align_to_cache, 64, 64)
    }

    /// Creates a new `Pool`.
    ///
    /// - `align_to_cache`: Should each object be on a separate CPU cache line. Speeds up
    /// multithreaded usage, but hurts single-threaded cache locality a bit and requires a bit more memory.
    /// Has no effect if `size_of::<T>` is already a multiple of a cache line size.
    /// 
    /// - `cache_line_size`: The size of an L1 cache line on the architecture. Must be a power of 2.
    /// 
    /// - `number_of_sets`: The number of [associativity](https://en.wikipedia.org/wiki/CPU_cache#Associativity) sets
    /// of the target processor. Decides the size of batch allocations.
    #[inline]
    pub fn with_system_params(align_to_cache: bool, cache_line_size: usize, number_of_sets: usize) -> Pool<T> {
        assert!(cache_line_size != 0, "Pool requested with cache_line_size = 0");
        assert!(number_of_sets != 0, "Pool requested with number_of_sets = 0");
        assert!(mem::size_of::<T>() != 0,
                "Pool requested with type of size 0");
        let batch_alignment = cache_line_size.max(mem::align_of::<T>());
        let align = ((mem::size_of::<T>() + mem::align_of::<T>() - 1) / mem::align_of::<T>()) * mem::align_of::<T>();
        let stride = if align_to_cache {
            ((cache_line_size + align - 1) / cache_line_size) * cache_line_size
        } else {
            align
        };
        let batch = (number_of_sets * cache_line_size / stride).max(1);
        let mem_size = batch * stride;
        let layout = Layout::from_size_align(mem_size, batch_alignment).expect("Pool requested with bad system cache parameters");
        Pool {
            data: Mutex::new(Vec::new()),
            free: MsQueue::new(),
            layout,
            batch,
            stride,
        }
    }

    /// Save the object on the heap. Will get a pointer that will drop it's content when
    /// dropped (like a `Box`). The memory will be reused though!
    ///
    /// Thread-safe. Very fast most of the time, but will take a bit longer if need to allocate
    /// more objects.
    ///
    /// Will panic if out of memory.
    #[inline]
    pub fn push(&self, obj: T) -> Object<T> {
        let slot = match self.free.try_pop() {
            Some(x) => x,
            None => self.expand(),
        };
        unsafe {
            ptr::write(slot, obj);
        }
        Object {
            obj: slot,
            manager: self,
        }
    }

    #[inline]
    fn expand(&self) -> *mut T {
        unsafe {
            let mut lock = self.data.lock().unwrap();
            if let Some(x) = self.free.try_pop() {
                return x;
            }
            let extra = Heap::default().alloc(self.layout.clone()).unwrap() as *mut T;
            // starting from 1 since index 0 will be returned
            for i in 1..self.batch {
                self.free.push((extra as usize + i * self.stride) as *mut T);
            }
            lock.push(extra);
            extra
        }
    }

    #[inline]
    fn ret_ptr(&self, obj: *mut T) {
        self.free.push(obj);
    }
}

impl<T> Default for Pool<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Drop for Pool<T> {
    #[inline]
    fn drop(&mut self) {
        let lock = match self.data.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        for block in lock.deref() {
            unsafe {
                Heap::default().dealloc(*block as *mut u8, self.layout.clone());
            }
        }
    }
}

impl<'active, T> Object<'active, T> {
    /// Returns the owned object from the pool-allocated memory.
    #[allow(needless_pass_by_value)]
    #[inline]
    pub fn recover(t: Self) -> T {
        let ret = unsafe {
            ptr::read(t.obj)
        };
        t.manager.ret_ptr(t.obj);
        ret
    }
}

impl<'active, T> Drop for Object<'active, T> {
    #[inline]
    fn drop(&mut self) {
        unsafe {
            ptr::read(self.obj);
        }
        self.manager.ret_ptr(self.obj);
    }
}

impl<'active, T> Deref for Object<'active, T> {
    type Target = T;

    #[allow(inline_always)]
    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        unsafe { &*self.obj }
    }
}

impl<'active, T> DerefMut for Object<'active, T> {
    #[allow(inline_always)]
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.obj }
    }
}

unsafe impl<'active, T: Send> Send for Object<'active, T> {}

unsafe impl<'active, T: Sync> Sync for Object<'active, T> {}

unsafe impl<T: Send> Send for Pool<T> {}

unsafe impl<T: Send> Sync for Pool<T> {}

impl<T> fmt::Debug for Pool<T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let pages = {
            self.data.lock().unwrap().len()
        };
        write!(f,
               "Pool {{ {} blocks, {} elements with {} stride in each. {} bytes allocated total for {} possible elements }}",
               pages,
               self.batch,
               self.stride,
               pages * self.layout.size(),
               pages * self.batch
               )
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
        let pool = Pool::with_params(false);
        let mut val2 = pool.push(val);
        assert_eq!(*val2, val);
        let val3 = 7u64;
        *val2 = val3;
        assert_eq!(*val2, val3);
    }
}
