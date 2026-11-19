# C-family cache reuse between checkouts

This fork keeps the object cache portable and the direct-mode cache local to the
physical compilation context. It does not rewrite compiler output or program
path strings. The changes are in C-family caching; Rust caching is unchanged.

Build this branch with Rust 1.91 or newer:

```sh
cargo build --release --locked
```

When switching from another sccache binary, stop its server before starting this
fork. Client and server binaries should match. Subsequent worktree changes do not
require restarting the server.

Set `SCCACHE_BASEDIRS` in the environment of each build to its source/build roots.
The value applies to that compile request, so a running server needs no restart
or pre-registration of future worktrees. With the variable absent, server roots
are used; an explicitly empty value disables normalization for that request.

Clang example in a project's CMake configuration:

```cmake
set(CMAKE_C_COMPILER_LAUNCHER /absolute/path/to/sccache)
set(CMAKE_CXX_COMPILER_LAUNCHER /absolute/path/to/sccache)
add_compile_options(
    "$<$<COMPILE_LANGUAGE:C,CXX,OBJC,OBJCXX>:-ffile-prefix-map=${CMAKE_BINARY_DIR}=/build>"
    "$<$<COMPILE_LANGUAGE:C,CXX,OBJC,OBJCXX>:-ffile-prefix-map=${CMAKE_SOURCE_DIR}=/source>"
    "$<$<COMPILE_LANGUAGE:C,CXX,OBJC,OBJCXX>:-fdebug-compilation-dir=/build>")
# For unity builds, when using CMake 4+:
set(CMAKE_UNITY_BUILD_RELOCATABLE ON)
```

```sh
export SCCACHE_BASEDIRS="/absolute/checkout:/absolute/build"
cmake --build /absolute/build
```

Prefix maps are compiler options: they make debug paths and compiler-generated
file names agree. Basedirs only normalize the cache key. Generated absolute
`#include` directives can still share an object after preprocessing, including
when the original checkout is unavailable. Program string data containing a
checkout path intentionally produces a different object key; relative metadata
can improve reuse without changing its meaning.

Binary PCH files keep their own input/build paths and remain local. PCH consumers
can share ordinary object files. Different unity source groupings are different
inputs and must miss; reproducible grouping belongs to the build generator.

The first lookup in another checkout preprocesses its current headers, creates
its current dependency file and may restore the shared object. Later local
lookups can skip preprocessing through the local direct-mode manifest. The
manifest key includes actual input/cwd, compiler and dependency arguments,
outputs and relevant compiler environment. Its local depfile is restored on a
direct hit instead of the depfile bundled with the shared object. A changed header is validated against
this checkout, and a cached depfile cannot be replayed into another build.

The C-family object and manifest cache namespaces have changed. Existing entries
are not used under the new semantics. There is no need to disable direct mode
for cross-checkout reuse in this fork.

Validation uses unit tests and `tests/cross_checkout.rs`: daemon/client-side
operation, current depfiles, header edits, absolute includes, runtime literals,
local PCH with an unavailable first checkout, and different output depfiles.
