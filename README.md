# VeryFast
`VeryFast` is a collection of useful tools needed mostly by game developers,
but fitting anyone who is focused on performance.
They are focused on speed and multithreaded safety.

At the moment it supplies one useful class - `Pool`, which allocates objects on the heap
like a `Box`, but allocates in batches and reuses the memory instead of deallocating
when Dropped! It is similar to `Arena` but allows deallocation.

###[Documentation](https://docs.rs/veryfast/)

##Nightly Requirements:
Nightly is required because of the next features:

- `#[feature(alloc, heap_api)]`: Custom allocation strategy for `Pool`

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

## 'SmallBuffer'

Disclaimer: Not yet ready

A small inline-allocated buffer with expansion capabilities. Pushing values can be done done asynchronously.
Reading values needs exclusive access. Removing values is only possible by draining the whole buffer.

The buffer is built like a linked list. Pushing many values at a time is discouraged. It fits well for cases where the
usual element count is low, but needs to be robust for the occasional peak.

# Awaited RFCs:

- [#1657 - const-dependent type system](https://github.com/rust-lang/rfcs/pull/1657): High potential for leverage of
the type system for speed