# Nova Language

A modern programming language with Swift-like syntax, Julia-style macros, algebraic data types, and pattern matching — that compiles to C++.

## Design

| Feature | Nova |
|---------|------|
| **Syntax** | Swift-like (braces, `func`, `let`/`var`), Scala-style generics `[T]` |
| **Macros** | Julia-style (quote/unquote, AST manipulation) |
| **Paradigm** | No OOP — ADTs + pattern matching |
| **Memory** | Stack by default, GC via `@T` syntax |
| **Compiles to** | C++ (with a bundled mark-sweep GC) |

## Language Reference

### Basics

```nova
// Variables
let x: Int = 42       // immutable
var y: Int = 42       // mutable
let z = 42            // type inference

// Functions
func add(a: Int, b: Int) -> Int {
    return a + b
}

// Control flow
if condition {
    // ...
} else {
    // ...
}

while condition {
    // ...
}

for item in collection {
    // ...
}
```

### Algebraic Data Types (ADTs)

```nova
enum Option[T] {
    case some(value: T)
    case none
}

enum Result[T, E] {
    case ok(value: T)
    case err(error: E)
}
```

### Pattern Matching

```nova
match value {
    case .some(let inner) => print(inner)
    case .none => print("nothing")
    case _ => print("default")
}
```

### Macros (Julia-style)

Macros operate on the AST using `quote` and `$unquote`:

```nova
macro make_adder(name, amount) {
    quote {
        func $name(x: Int) -> Int {
            return x + $amount
        }
    }
}

@make_adder(add_five, 5)
@make_adder(add_ten, 10)
```

- `quote { ... }` — captures code as AST
- `$ident` — interpolates an AST value
- `$(expr)` — evaluates and splices an AST expression
- `@macroname(args)` — invokes a macro

### GC References (`@T`)

```nova
struct Node {
    value: Int
    next: @Node    // GC-managed reference
}

func make_node(val: Int) -> @Node {
    return @Node { value: val, next: @Node {} }
}
```

- Regular types are stack-allocated
- `@T` is a garbage-collected heap reference
- The runtime includes a mark-sweep collector

### Module System (via `@import` macro)

The module system piggybacks on the macro system — `@import` is just a built-in macro:

```nova
// Import everything public from a module
@import("std.math")

// Import specific items
@import("std.math", square, cube)

// Import with renaming
@import("std.math", square = sq, cube = cb)

// Use imported functions
let result = square(5);
let renamed = sq(3);
```

Modules are resolved from:
1. The current file's directory
2. Paths specified with `-L` flag
3. The current working directory

A module `foo.bar` looks for `foo/bar.nv` or `foo/bar/mod.nv`.

```nova
struct Point {
    x: Float
    y: Float
}

let p = Point { x: 1.0, y: 2.0 }
```

## Compiler Architecture

```
Source (.nv)
    │
    ▼
┌─────────┐
│  Lexer  │  → Token stream
└─────────┘
    │
    ▼
┌─────────┐
│ Parser  │  → AST (recursive descent)
└─────────┘
    │
    ▼
┌───────────────┐
│ Macro Expander│  → AST with macros expanded
└───────────────┘
    │
    ▼
┌──────────────────────┐
│ Monomorphizer        │  → generics specialized per instantiation
└──────────────────────┘
    │
    ▼
┌──────────────┐
│ Type Checker │  → Validated AST
└──────────────┘
    │
    ▼
┌──────────────┐
│ C++ Codegen  │  → .cpp output
└──────────────┘
```

### Generics (monomorphized)

Generic functions, structs and enums are specialized by the compiler: every
instantiation becomes a concrete, non-generic C++ definition
(`Option[Int]` becomes `Option__int`, a call `id(42)` compiles to `id__int(42)`),
so user code never relies on C++ templates. Type arguments are inferred at each
call site from the arguments, and from annotations when a payload cannot pin
every parameter:

```nova
enum Option[T] { case some(T), case none }
struct Pair[A, B] { a: A, b: B }

func id[T](x: T) -> T { return x }

func swap[A, B](p: Pair[A, B]) -> Pair[B, A] {
    return Pair { a: p.b, b: p.a }
}

let o: Option[Pair[Int, Bool]] = .some(Pair { a: 1, b: true })
let n: Option[Int] = .none     // payload-less constructors read the context
```

Only reachable instantiations are emitted; dead generic code is dropped.

## Building

```bash
cargo build --release
```

## Usage

```bash
# Compile a Nova file to C++
./target/release/nova examples/hello.nv -o hello.cpp

# Print the AST
./target/release/nova --print-ast examples/macro.nv

# Type-check only
./target/release/nova --check-only examples/adt.nv

# Standalone mode (bundles GC runtime)
./target/release/nova --standalone examples/gc_refs.nv -o gc_demo.cpp
```

## Examples

See the `examples/` directory:

- `hello.nv` — Hello World
- `adt.nv` — ADTs and pattern matching  
- `macro.nv` — Julia-style macros
- `gc_refs.nv` — GC-managed data structures
- `test_min.nv` — Minimal working example
- `test_macro.nv` — Macro expansion demo
- `test_generic.nv` — Generic functions/structs/enums (monomorphized)

## Runtime

The GC runtime (`rt/gc.h`) provides:
- `nova::gc_ptr<T>` — smart pointer for `@T` references
- `nova::gc_alloc<T>(args...)` — allocate a GC-managed object
- Mark-sweep collection with configurable threshold

## Status

**v0.1** — Working prototype with:
- ✅ Full lexer and parser
- ✅ Julia-style macro system with quote/unquote
- ✅ ADTs with pattern matching
- ✅ First-order generics (functions, structs, enums) with Nova-level monomorphization
- ✅ GC references (`@T`)
- ✅ C++ code generation
- ✅ Module system via `@import` macro (with renaming)
- ✅ Basic type checking (with extern function passthrough)
- 🚧 Standard library (print, collections)
- 🚧 Higher-kinded types / full type inference
- 🚧 Pattern exhaustiveness checking
