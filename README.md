# Status Update
Progress is blocked by missing features of rust.

- [Const generics](https://github.com/rust-lang/rust/issues/44580): Specifying the buffer size for both
`Pool` and `SmallBuffer` is impossible without it. For now a default has been set, but might not fit every use.

## Nightly Requirements:
Nightly is required because of the next features:

- `#![feature(allocator_api)]`: Custom alignment for `Pool` allocations

# VeryFast
`VeryFast` is a collection of useful tools needed mostly by game developers,
but fitting anyone who is focused on performance.
They are focused on speed and multithreaded safety.

At the moment it supplies one useful class - `Pool`, which allocates objects on the heap
like a `Box`, but allocates in batches and reuses the memory instead of deallocating
when Dropped! It is similar to `Arena` but allows deallocation.

### [Documentation](https://docs.rs/veryfast/)

# Tools

## `Pool`

`Pool` allocates objects on the heap in batches. All objects must be of the same type like in `Vec`.
It allows fast multithreaded parallel allocation of objects.
When objects are dropped, their memory is returned to the pool to be reused for future allocations.
Only when all the objects and the `Pool` are dropped will the memory be released.

There are some optimisations for speed: It is possible to allocate each object on a separate CPU cache line
(helps with multithreaded access to adjacent elements). The allocations use a lock-free strategy except when
allocating a new batch. Deallocations also use a lock-free strategy.

The `Pool` is similar to various [`Arena`](https://github.com/SimonSapin/rust-typed-arena) implementations but it
allows deallocation of elements and reuse of the memory.

## `SmallBuffer`

A small inline-allocated buffer with expansion capabilities. Pushing values can be done done asynchronously.
Reading values needs exclusive access. Removing values is only possible by draining the whole buffer.

`SmallBuffer` is useful as a buffer for elements that see little usage.
It has a small capacity inline, so a couple messages will not cause it to allocate memory.
If it receives more data than it can store, it will allocate additional memory to handle it.
It will not deallocate any memory, for cases when it's likely an element that has seen a lot of
usage has a higher chance to continue having high usage.