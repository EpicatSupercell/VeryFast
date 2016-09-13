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
//! ```
//! use veryfast::pool::{Pool, Object};
//!
//! let pool = Pool::new(false, 1000);
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
//!# extern crate veryfast;
//! extern crate scoped_threadpool;
//!
//! fn slow(val: &mut i32) {
//!     *val += 1;
//! }
//!
//! let mut thread_pool = scoped_threadpool::Pool::new(4);
//! let memory_pool = veryfast::pool::Pool::new(true, 1000);
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
use std::sync::{Mutex, Arc};

use std::mem;
use std::ptr;
use alloc::heap;
use std::ops::{Deref, DerefMut};
use std::fmt;
use std::marker::PhantomData;

use super::crossbeam::sync::MsQueue;

/// A fast heap-allocator. Allocates objects in a batch, but transfers the control to the `Object`.
///
/// Allocations will first check if there is an already free slot to use, and use that.
/// If no, It will take a lock and allocate a batch of memory.
///
/// When objects are dropped, their memory will be returned to the pool to be used again later.
/// The memory of the batches will be deallocated only when the `Pool` and all the related `Object`s
/// are dropped.
pub struct Pool<TYPE> {
    manager: Arc<Manager<TYPE>>,
}

/// A pointer type that owns its content.
///
/// Created from a `Pool`. The `Object` owns the value inside it and has exclusive access to it.
///
pub struct Object<TYPE> {
    obj: *mut TYPE,
    manager: Arc<Manager<TYPE>>,
    _marker: PhantomData<TYPE>,
}

struct Manager<TYPE> {
    data: Mutex<Vec<*const TYPE>>,
    free: MsQueue<*mut TYPE>,
    batch: usize,
    align: usize,
    memory_size: usize,
    elem_size: usize,
}

impl<TYPE> Pool<TYPE> {
    /// Creates a new `Pool`.
    ///
    /// - `batch`: How many objects should be allocated each time. Higher numbers are faster,
    /// but can cause wasted memory if too little are actually used.
    ///
    /// - `align_to_cache`: Should each object be on a separate CPU cache line. Speeds up
    /// multithreaded usage but requires a bit more memory in most cases.
    #[inline]
    pub fn new(batch: usize, align_to_cache: bool) -> Pool<TYPE> {
        Pool { manager: Arc::new(Manager::new(align_to_cache, batch)) }
    }

    /// Save the object on the heap. Will get a pointer that will drop it's content when
    /// dropped (like a `Box`). The memory will be reused though!
    ///
    /// Thread-safe. Very fast most of the time, but will take a bit longer if need to allocate
    /// more objects.
    #[inline]
    pub fn insert(&self, obj: TYPE) -> Object<TYPE> {
        self.manager.insert(obj, self.manager.clone())
    }
}

impl<TYPE> Manager<TYPE> {
    #[inline]
    pub fn new(align_to_cache: bool, batch: usize) -> Manager<TYPE> {
        let mut align = mem::align_of::<TYPE>();
        let mut elem_size = mem::size_of::<TYPE>();
        if align_to_cache {
            let cache_line_size = 64;
            align = ((align - 1) / cache_line_size + 1) * cache_line_size;
            elem_size = ((elem_size - 1) / cache_line_size + 1) * cache_line_size;
        }
        let memory_size = elem_size * batch;
        Manager::<TYPE> {
            data: Mutex::new(Vec::new()),
            free: MsQueue::new(),
            batch: batch,
            align: align,
            memory_size: memory_size,
            elem_size: elem_size,
        }
    }

    #[inline]
    fn expand(&self) -> *mut TYPE {
        unsafe {
            let extra = heap::allocate(self.memory_size, self.align) as *mut TYPE;
            if extra.is_null() {
                panic!("out of memory");
            }
            // starting from 1 since index 0 will be returned
            for i in 1..self.batch {
                self.free.push((extra as usize + i * self.elem_size) as *mut TYPE);
            }
            self.data.lock().unwrap().push(extra);
            extra
        }
    }

    #[inline]
    pub fn insert(&self, obj: TYPE, manager: Arc<Manager<TYPE>>) -> Object<TYPE> {
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
            _marker: PhantomData,
        }
    }

    #[inline]
    pub fn ret_ptr(&self, obj: *mut TYPE) {
        self.free.push(obj);
    }
}

impl<TYPE> Drop for Manager<TYPE> {
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

impl<TYPE> Object<TYPE> {}

impl<TYPE> Drop for Object<TYPE> {
    #[inline]
    fn drop(&mut self) {
        unsafe {
            ptr::read(self.obj);
        }
        (*self.manager).ret_ptr(self.obj);
    }
}

impl<TYPE> Deref for Object<TYPE> {
    type Target = TYPE;

    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        unsafe { &*self.obj }
    }
}

impl<TYPE> DerefMut for Object<TYPE> {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.obj }
    }
}

unsafe impl<TYPE> Send for Object<TYPE> where TYPE: Send {}

unsafe impl<TYPE> Sync for Object<TYPE> where TYPE: Sync {}

unsafe impl<TYPE> Send for Manager<TYPE> where TYPE: Send {}

unsafe impl<TYPE> Sync for Manager<TYPE> where TYPE: Sync {}

impl<TYPE> fmt::Debug for Pool<TYPE> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f,
               "{} objects in {:?}",
               Arc::strong_count(&self.manager) - 1,
               self.manager)
    }
}

impl<TYPE> fmt::Debug for Manager<TYPE> {
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

/*impl<TYPE> fmt::Debug for Object<TYPE>
    where TYPE: fmt::Debug
{
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        (**self).fmt(f)
    }
}

impl<TYPE> fmt::Display for Object<TYPE>
    where TYPE: fmt::Display
{
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        (**self).fmt(f)
    }
}*/

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_dereference() {
        let val = 5u64;
        let pool = Pool::new(false, 10);
        let mut val2 = pool.insert(val);
        assert_eq!(*val2, val);
        let val3 = 7u64;
        *val2 = val3;
        assert_eq!(*val2, val3);
    }
}
