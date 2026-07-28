# The Lilac Programming Language Specification

Lilac is an ultra-lean systems programming language designed around compiler minimalism. The core compiler implements only raw bit containers, control flow, functions, generics, and the foundational building blocks required to define custom types. High-level abstractions—such as strings, dynamic arrays, floating-point types, optionals, and mutability guards—are deliberately excluded from the compiler and implemented within the standard library.

## 1. Core Types & Memory Model

Lilac operates on raw memory containers rather than semantic primitives. The compiler attaches no inherent numeric meaning to types; interpretation is deferred entirely to the functions acting upon them.

### Primitive Bit Containers

* **`@bit8`**, **`@bit16`**, **`@bit32`**, **`@bit64`**: Fixed-width raw memory containers. Whether a container represents a two's-complement integer, an IEEE-754 float, or arbitrary data depends on the intrinsic function invoked (e.g., `@iadd` vs. `@fadd`).

* **`@bitNative`**: A container sized to the target architecture's native word length (equivalent to `usize`/`uintptr_t`).

* **`@numberLiteral`**: A compile-time reference to the UTF-8 representation of a numeric literal. Compiler intrinsics (such as `@asInt()` or `@asFloat()`) evaluate the literal at compile time and strip the reference.

### Special Keywords

* **`unknown`**: An opaque type of indeterminate size. It can only be instantiated behind a reference (`ref unknown`), serving as Lilac's mechanism for type-erased pointers (analogous to `void*`).

* **`nothing`**: A parse-time keyword used exclusively in a function's return slot to indicate zero return variables. It is not a type and occupies no memory.

### Universal Zero-Initialization

All allocated memory—whether on the stack, heap, or within complex objects—is automatically **0-initialized** by the compiler. There is no uninitialized memory in Lilac. This eliminates read-before-write bugs and guarantees deterministic padding bits, ensuring 100% reliability for bit-equality checks.

### Copy Semantics

Lilac does not implement move semantics. Value assignment and parameter passing always execute a **fast bitwise copy** by default. Custom copy behavior (such as deep copying or reference counting) is implemented via RAII hooks.

## 2. Literals & Desugaring

String and number literals desugar into standard library object representations at compile time.

Number literals hold a compile-time reference to their UTF-8 string

```lilac
NumberLiteral: obj {
    Size: @bit32
    Buffer: ref @bit8
}
```

String literals are pointers to a UTF-8 character array with an explicit size

```lilac
String: obj {
    Size: @bit32
    Buffer: ref @bit8
}
```

Formatted strings (`$"Hello {name}"`) desugar into structured objects.
A `.toString()` method on the object generates the final UTF-8 output:

```lilac
FString: obj {
    Size_1: @bit32
    Name: String
    Buffer_1: ref @bit8
}
```

## 3. Variables, References & Immutability

### Variable Declaration

Variables are declared using `name: Type = value` syntax. Object types can utilize simplified constructor syntax during initialization, or invoke parameterized constructors using `Type(params)` if a matching `@on_init` hook is defined.

```lilac
counter: @bit32 = 0x0
user: Object {}
custom_user: Object(0x1, "Lilac") // Invokes @on_init with custom params
```

The base language enforces no concept of mutability. Read-only variables and immutability guards are implemented in user-land using the `@on_override` compiler hook.

### References & Auto-Dereferencing

References are denoted by the **`ref`** keyword. In Lilac, `ref` acts as a type modifier flag rather than a wrapper container. Because a type cannot be flagged as a reference twice, double references (e.g., `ref ref @bit32`) are syntactically invalid unless wrapped inside an object.

```lilac
variable: @bit32 = 0x0
reference: ref @bit32 = ref variable
```

References **auto-dereference by default**. Operators act on the underlying value, not the memory address:

* `ptr + 1` invokes `@add` on the underlying `@bit32`, incrementing the integer value by `1`.

* It does **not** perform pointer arithmetic. To manipulate raw memory addresses, a reference must be explicitly converted to `@bitNative`.

## 4. Composite Types & Namespaces

### Objects

An **`obj`** defines both a custom data layout and a namespace. Functions declared inside an object are public members of that namespace by default.

```lilac
Object: obj {
    Member: @bit32

    // Static function (no instance parameter)
    StaticFunction: func () nothing {}
    
    // Instance method using 'self' syntactic sugar (self: ref Object)
    MemberFunction: func (self) nothing {}
}
```

### Namespaces & Visibility

Namespaces mirror the file system directory structure and object hierarchies. Everything inside a namespace is mutually visible. The **`using`** keyword imports a target namespace into the current scope; imports are non-transitive and do not pull in parent directories.

```lilac
using Standard
```

### Enums and Tagged Unions

Enums and unions are syntactic sugar that desugar into standard objects with explicit memory layouts. The `=>` operator maps constructor branches to their resulting object state.

```lilac
Enum: enum { One, Two }
```

Desugared object equivalent:

```lilac
Enum: obj {
    value: @bit8
    One: Enum => Enum { value = 0x0 }
    Two: Enum => Enum { value = 0x1 }
}
```

Unions are tagged by default. The compiler allocates memory equal to the size of the largest variant plus the tag enum.

```lilac
Union: union {
    Some: @bit8
    None
}
```

Desugared object equivalent:

```lilac
Union: obj {
    Tag: enum { Some, None }
    tag: Tag
    data: @bit8 // Sized to fit the largest member

    Some: func (value: @bit8) Union => Union {
        tag = .Some
        data = value
    }
    None: Union => Union { tag = .None }
}
```

### Interfaces & VTables

Interfaces are **shape-based** (structural typing). Types do not explicitly declare interface implementation. When an object is assigned to an interface variable, the compiler validates the structural match and generates a virtual method table (VTable) automatically.

```lilac
Reader: interface {
    read: func () nothing
}

// Compiler verifies 'File' implements 'read' and generates the VTable
stream: Reader = File
```

## 5. Functions & Control Flow

### Function Signatures & Multi-Return

To return values from a function, named return variables **must** be specified in the signature. The `return` keyword takes no arguments; it simply returns the current state of the named return variables.

```lilac
multi_func: func () result_1: @bit8, result_2: @bit16 {
    result_1 = 0x1
    result_2 = 0x2
    return
}

val1: @bit8, val2: @bit16 = multi_func()
```

### Control Flow

Conditional statements evaluate expressions as boolean based on strict non-zero testing against `@bit8` (`0x0` is false; any other value is true). Non-`@bit8` types must be explicitly or implicitly converted to `@bit8` before evaluation.

If expressions take a value to conditionally call code:

```lilac
if condition {

}
```

You can also attach else blocks to ifs:

```lilac
if condition {
    
} else {

}
```

If expressions can also return values:

```lilac
status: @bit8 = if 0x0 0x0 else 0x1
```

Value capturing from multi-return functions:
If the first return value is non-zero, subsequent values are captured into scope:

```lilac
if multi_func -> secondary_val: @bit16 {
    // secondary_val is scoped here
}
```

Loops execute infinitely by default unless broken or conditioned:

```lilac
loop {
    // This will loop infinitely.
}
```

To make a standard conditional loop this is a way:

```lilac
val: @bit8 = 0x0
loop if @iLessThan(val, 0x1) {
    // do work
} else break
```

### Pattern Matching

The **`match`** statement inspects union tags and byte patterns. Match statements are **not required to be exhaustive**. If no valid branch matches, execution silently falls through (equivalent to an implicit empty `else {}` branch).

```lilac
match union_val {
    .Some -> value: @bit16 {
        return
    }
    .None => return
}
```

You can attach else to the end of a match to catch invalid matches:

```lilac
match union_val {
    .None => return
} else {
    return
}
```

## 6. Generics & Inline Declarations

### Generics

Generics provide compile-time templating for any declaration. The compiler ignores the generic parameter `T` until the declaration is instantiated, allowing duck-typing within generic functions.

```lilac
GenObj[T]: obj {
    value: T
}

GenFunc[T]: func (value: T) nothing {
    // Validated only at instantiation time
    value.call_something() 
}
```

### Inline Declarations

The **`inlined`** keyword defines single-expression declarations that cannot contain nested inline rules. Inlined functions may omit the return name but are restricted to a single output type. The `=>` operator directly binds the expression output.

```lilac
inlined_const: @bit64 => 0x0
inlined_func: func (val: @bit8) @bit8 => @iadd(val, 0x1)
```

## 7. Operators, Conversions & RAII Hooks

Lilac uses designated `@`-prefixed function names to bind global behaviors to types. Multiple bindings can be defined across a program provided their type signatures `(from, to)` or `(lhs, rhs)` remain unique.

### Operators & Strict Equality

Binary and unary operators desugar directly into function calls. For example, `a += b` desugars to `a = @add(a, b)`.

```lilac
@add: func (lhs: @bit8, rhs: @bit8) @bit8 => @iadd(lhs, rhs)
```

**Equality (`==`) is strictly bit-equality and cannot be overridden.** Because universal zero-initialization guarantees that unused memory and struct padding are always zeroed, bitwise comparison is 100% deterministic. Custom comparison logic must be implemented as regular named methods to prevent hidden runtime complexity.

### Type Conversions

Conversions are unidirectional from left to right. The compiler automatically searches for a valid conversion binding when resolving type expectations.

```lilac
@conversion: func (from: CustomType) to: @bit8
```

### Resource Management (RAII)

Memory lifecycle events trigger specific compiler hooks if defined for a type.

| Hook | Signature | Invocation Trigger & Behavior Rules |
| --- | ------ | ------ |
| **`@on_init`** | `(...params) Type` | Called immediately after object instantiation. Can be defined with custom parameters and invoked directly as a constructor using the syntax `Type(params)`.|
| **`@on_override`** | `(ref prev, ref new) nothing` | Called **before** an existing variable's value is overwritten. **Rule:** The `@on_drop` hook is automatically invoked on `prev` immediately afterwards, so you must **not** override or free anything in `prev` during `@on_override`.|
| **`@on_copy`** | `(self) new_copy` | Called when a value is copied. You are manually responsible for creating and returning the new copied value (`new_copy`). **Rule:** Extreme care must be taken not to recursively trigger the copy mechanism while constructing `new_copy`.|
| **`@on_drop`** | `(self) nothing` | Called when a value exits scope or a reference is freed. Used for resource deallocation and cleanup.|

```lilac
// Example of defining custom constructors and RAII rules
Buffer: obj {
    ptr: ref @bit8

    // Custom constructor called via Buffer(size)
    @on_init: func (self, size: @bit32) nothing {
        // allocation logic
    }

    // Called before override; do not free 'prev' here as @on_drop handles it!
    @on_override: func (prev: ref Buffer, new: ref Buffer) nothing {
        // transfer logic or immutability enforcement
    }

    // Responsible for generating the copy without causing infinite copy recursion
    @on_copy: func (self) new_copy: Buffer {
        // safe copy logic
        return
    }
}
```

## 8. Compiler Intrinsics

Intrinsics represent the sole mechanisms for raw arithmetic, float manipulation, and bit-level introspection.

| Intrinsic | Signature | Description |
| --- | --- | --- |
| **`@iadd`** | `(lhs, rhs)` | Integer addition using standard two's-complement logic.|
| **`@fadd`** | `(lhs, rhs)` | Floating-point addition using IEEE-754 bit interpretation.|
| **`@isub`** / **`@fsub`** | `(lhs, rhs)` | Integer and floating-point subtraction.|
| **`@imul`** / **`@fmul`** | `(lhs, rhs)` | Integer and floating-point multiplication.|
| **`@idiv`** / **`@fdiv`** | `(lhs, rhs)` | Integer and floating-point division.|
| **`@imod`** / **`@fmod`** | `(lhs, rhs)` | Integer and floating-point modulo arithmetic.|
| **`@asInt`** | `(bits)` | Reinterprets a `@numberLiteral` or bit container as an integer.|
| **`@asFloat`** | `(bits)` | Reinterprets a `@numberLiteral` or bit container as a float.|
| **`@sizeOf`** | `(value)` | Returns the size of a type or variable in bytes as a `@bitNative`.|