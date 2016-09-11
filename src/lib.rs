#![feature(alloc, heap_api)]
#![feature(arc_counts)]
#![feature(plugin)]
#![plugin(clippy)]

//! `VeryFast` is a collection of useful tools needed mostly by game developers.
//! It is designed to work well in multi threaded contexts.
//!
//! #Examples
//! ```
//! use veryfast::pool::{Pool, Object};
//!
//! let pool = Pool::new(true, 1000);
//!
//! let var1 = pool.add(15i32);
//! let mut var2 = pool.add(7);
//! *var2 = *var1;
//! assert_eq!(*var1, *var2);
//!
//! let mut vec = Vec::new();
//! for i in 0..10 {
//!     vec.push(pool.add(i));
//! }
//! for i in &vec {
//!     print!("{} ", **i);
//! }
//! ```
//!
//! #Nightly Requirements:
//! Nightly is required for the next features:
//!
//! - `#[feature(alloc)]`: Custom allocation
//!
//! - `#[feature(heap_api)]`: Custom allocation

extern crate alloc;
extern crate crossbeam;

pub mod pool;

// possible future crates
//
// scoped_threadpool = "*"
// num_cpus = "*"
// futures = { git = "https://github.com/alexcrichton/futures-rs" }
