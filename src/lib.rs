#![deny(missing_docs)]
#![allow(unknown_lints)]

//! `VeryFast` is a collection of useful tools needed mostly by game developers.
//! It is designed to work well in multi threaded contexts.
//!
//! At the moment it supplies one useful class - `pool::Pool`, which allocates objects on the heap
//! like a `Box`, but allocates in batches and reuses the memory instead of deallocating
//! when Dropped!
//!
//! ##Nightly Requirements:
//! Nightly is required because of the next features:
//!
//! - `#[feature(alloc, heap_api)]`: Custom allocation strategy for `Pool`

extern crate crossbeam;

pub mod pool;
pub mod small_buffer;

//mod tiny_buffer;
// mod internal;
