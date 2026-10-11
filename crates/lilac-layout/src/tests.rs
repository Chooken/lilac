use lilac_target::{Scalar, Target};

use crate::{array_layout, layout, struct_layout, tagged_layout, Layout, Part, Shape, SizeAlign};

fn target(triple: &str) -> Target {
    Target::parse(triple).expect("valid triple")
}

fn x86_64() -> Target {
    target("x86_64-unknown-linux-gnu")
}

fn aarch64() -> Target {
    target("aarch64-apple-darwin")
}

fn i686() -> Target {
    target("x86-unknown-linux-gnu")
}

fn s(scalar: Scalar) -> Shape {
    Shape::Scalar(scalar)
}

fn of(target: Target, shape: &Shape) -> Layout {
    layout(&target, shape).expect("fits")
}

fn sized(size: u64, align: u64) -> SizeAlign {
    SizeAlign { size, align }
}

/// `(size, align, offsets)` of a struct of two scalars.
fn pair(target: Target, a: Scalar, b: Scalar) -> (u64, u64, Vec<u64>) {
    let layout = of(target, &Shape::Struct(vec![s(a), s(b)]));
    (layout.size, layout.align, layout.fields)
}

#[test]
fn scalar_pairs_on_x86_64_and_aarch64() {
    for target in [x86_64(), aarch64()] {
        assert_eq!(
            pair(target, Scalar::Bit8, Scalar::Bit64),
            (16, 8, vec![0, 8])
        );
        assert_eq!(
            pair(target, Scalar::Bit64, Scalar::Bit8),
            (16, 8, vec![0, 8])
        );
        assert_eq!(
            pair(target, Scalar::Bit8, Scalar::Bit16),
            (4, 2, vec![0, 2])
        );
        assert_eq!(
            pair(target, Scalar::Bit16, Scalar::Bit32),
            (8, 4, vec![0, 4])
        );
        assert_eq!(
            pair(target, Scalar::Float16, Scalar::Float32),
            (8, 4, vec![0, 4])
        );
        assert_eq!(
            pair(target, Scalar::Bit8, Scalar::Float128),
            (32, 16, vec![0, 16])
        );
        assert_eq!(
            pair(target, Scalar::Bit32, Scalar::Pointer),
            (16, 8, vec![0, 8])
        );
        assert_eq!(
            pair(target, Scalar::Float64, Scalar::Bit8),
            (16, 8, vec![0, 8])
        );
    }
}

#[test]
fn scalar_pairs_on_i686_linux() {
    let target = i686();
    // i386 System V aligns 8-byte scalars to 4.
    assert_eq!(
        pair(target, Scalar::Bit8, Scalar::Bit64),
        (12, 4, vec![0, 4])
    );
    assert_eq!(
        pair(target, Scalar::Bit8, Scalar::Float64),
        (12, 4, vec![0, 4])
    );
    assert_eq!(
        pair(target, Scalar::Bit64, Scalar::Bit8),
        (12, 4, vec![0, 8])
    );
    assert_eq!(
        pair(target, Scalar::Bit8, Scalar::Pointer),
        (8, 4, vec![0, 4])
    );
    assert_eq!(
        pair(target, Scalar::Bit8, Scalar::Float128),
        (32, 16, vec![0, 16])
    );
    assert_eq!(
        pair(target, Scalar::Bit16, Scalar::Bit8),
        (4, 2, vec![0, 2])
    );
    // Windows keeps them at 8.
    let windows = target_windows_x86();
    assert_eq!(
        pair(windows, Scalar::Bit8, Scalar::Bit64),
        (16, 8, vec![0, 8])
    );
}

fn target_windows_x86() -> Target {
    target("x86-pc-windows-msvc")
}

/// Every scalar after every other, on each 64-bit and 32-bit flavor: the
/// second field sits at the first's size rounded up to its alignment, and
/// the size is the end rounded up to the larger alignment.
#[test]
fn every_scalar_after_every_other() {
    for target in [x86_64(), aarch64(), i686(), target_windows_x86()] {
        for a in Scalar::ALL {
            for b in Scalar::ALL {
                let (a_size, a_align) = target.scalar_layout(a);
                let (b_size, b_align) = target.scalar_layout(b);
                let offset = a_size.next_multiple_of(b_align);
                let align = a_align.max(b_align);
                let size = (offset + b_size).next_multiple_of(align);
                assert_eq!(
                    pair(target, a, b),
                    (size, align, vec![0, offset]),
                    "{target} {a:?} {b:?}"
                );
            }
        }
    }
}

#[test]
fn empty_struct_is_size_zero_align_one() {
    let empty = of(x86_64(), &Shape::Struct(Vec::new()));
    assert_eq!(
        empty,
        Layout {
            size: 0,
            align: 1,
            fields: Vec::new()
        }
    );
    // An empty struct between fields takes no space.
    let between = of(
        x86_64(),
        &Shape::Struct(vec![
            s(Scalar::Bit8),
            Shape::Struct(Vec::new()),
            s(Scalar::Bit8),
        ]),
    );
    assert_eq!((between.size, between.fields), (2, vec![0, 1, 1]));
}

#[test]
fn trailing_padding_and_nesting() {
    let inner = Shape::Struct(vec![s(Scalar::Bit64), s(Scalar::Bit8)]);
    assert_eq!(of(x86_64(), &inner).size, 16);
    assert_eq!(of(i686(), &inner).size, 12);
    let outer = Shape::Struct(vec![s(Scalar::Bit8), inner.clone(), s(Scalar::Bit16)]);
    let on_64 = of(x86_64(), &outer);
    assert_eq!(
        (on_64.size, on_64.align, on_64.fields),
        (32, 8, vec![0, 8, 24])
    );
    let on_32 = of(i686(), &outer);
    assert_eq!(
        (on_32.size, on_32.align, on_32.fields),
        (20, 4, vec![0, 4, 16])
    );
}

#[test]
fn arrays_are_c_arrays() {
    let elem = Shape::Struct(vec![s(Scalar::Bit64), s(Scalar::Bit8)]);
    let array = of(x86_64(), &Shape::Array(Box::new(elem.clone()), 3));
    assert_eq!((array.size, array.align), (48, 8));
    let holder = of(
        i686(),
        &Shape::Struct(vec![s(Scalar::Bit8), Shape::Array(Box::new(elem), 2)]),
    );
    assert_eq!(
        (holder.size, holder.align, holder.fields),
        (28, 4, vec![0, 4])
    );
    let nested = of(
        x86_64(),
        &Shape::Array(Box::new(Shape::Array(Box::new(s(Scalar::Bit16)), 3)), 5),
    );
    assert_eq!((nested.size, nested.align), (30, 2));
}

#[test]
fn tagged_unions() {
    let x86_64 = x86_64();
    // No payloads: just the tag.
    let bare = tagged_layout(&x86_64, Scalar::Bit8, &[]).expect("fits");
    assert_eq!(
        bare,
        Layout {
            size: 1,
            align: 1,
            fields: vec![0]
        }
    );
    let bare = tagged_layout(&x86_64, Scalar::Bit16, &[]).expect("fits");
    assert_eq!((bare.size, bare.align), (2, 2));
    // The payload area starts at the strictest payload alignment.
    let mixed = tagged_layout(&x86_64, Scalar::Bit8, &[sized(8, 8), sized(1, 1)]).expect("fits");
    assert_eq!(
        mixed,
        Layout {
            size: 16,
            align: 8,
            fields: vec![0, 8, 8]
        }
    );
    let short = tagged_layout(&x86_64, Scalar::Bit16, &[sized(3, 1)]).expect("fits");
    assert_eq!(
        short,
        Layout {
            size: 6,
            align: 2,
            fields: vec![0, 2]
        }
    );
    // Problem 5: three 256-byte payloads share one area.
    let bytes = sized(256, 1);
    let union = tagged_layout(&x86_64, Scalar::Bit8, &[bytes, bytes, bytes]).expect("fits");
    assert_eq!((union.size, union.align), (257, 1));
    // i686 System V: a double payload is aligned to 4.
    let on_32 = tagged_layout(
        &i686(),
        Scalar::Bit8,
        &[SizeAlign::scalar(&i686(), Scalar::Float64)],
    )
    .expect("fits");
    assert_eq!((on_32.size, on_32.align, on_32.fields), (12, 4, vec![0, 4]));
    // An empty payload adds no size past the tag's.
    let empty = tagged_layout(&x86_64, Scalar::Bit32, &[sized(0, 1)]).expect("fits");
    assert_eq!((empty.size, empty.fields), (4, vec![0, 4]));
    // The tree form agrees.
    let tree = of(
        x86_64,
        &Shape::Tagged {
            tag: Scalar::Bit8,
            payloads: vec![s(Scalar::Bit64), s(Scalar::Bit8)],
        },
    );
    assert_eq!(tree, mixed);
}

#[test]
fn the_limit_is_exact() {
    let x86_64 = x86_64();
    let limit = x86_64.max_object_size();
    let byte = sized(1, 1);
    assert_eq!(array_layout(&x86_64, byte, limit), Ok(sized(limit, 1)));
    let past = array_layout(&x86_64, byte, limit + 1).expect_err("too large");
    assert_eq!(past.part, Part::Length);
    assert_eq!(past.size, u128::from(limit) + 1);
    assert_eq!(past.limit, limit);

    // A field ending exactly at the limit fits; one more byte does not.
    let fits = struct_layout(&x86_64, &[byte, sized(limit - 1, 1)]).expect("fits");
    assert_eq!(fits.size, limit);
    let crossed =
        struct_layout(&x86_64, &[byte, sized(limit - 1, 1), byte]).expect_err("too large");
    assert_eq!(crossed.part, Part::Field(2));
    // Trailing padding can cross it too: blamed on the last field.
    let padded = struct_layout(&x86_64, &[sized(8, 8), sized(limit - 8, 1)]).expect_err("padding");
    assert_eq!(
        (padded.part, padded.size),
        (Part::Field(1), u128::from(limit) + 1)
    );

    // Problem 2: no wrap, the exact size reached.
    let row = array_layout(&x86_64, sized(8, 8), 0xFFFF_FFFF).expect("34 GB fits");
    let big = array_layout(&x86_64, row, 0xFFFF_FFFF).expect_err("too large");
    assert_eq!(big.size, 8 * 0xFFFF_FFFF_u128 * 0xFFFF_FFFF);
    assert_eq!(
        big.to_string(),
        format!(
            "{} bytes is larger than the target's limit of {limit} bytes",
            big.size
        )
    );
    let huge = array_layout(&x86_64, sized(u64::MAX, 1), u64::MAX).expect_err("too large");
    assert_eq!(huge.size, u128::from(u64::MAX) * u128::from(u64::MAX));

    // 32-bit targets stop at 2^31 - 1.
    let wasm = target("wasm32-unknown-unknown");
    assert!(array_layout(&wasm, byte, 0x7FFF_FFFF).is_ok());
    assert!(array_layout(&wasm, byte, 0x8000_0000).is_err());
    let i686 = i686();
    let crossed =
        tagged_layout(&i686, Scalar::Bit8, &[byte, sized(0x7FFF_FFFF, 1)]).expect_err("too large");
    assert_eq!((crossed.part, crossed.size), (Part::Field(2), 0x8000_0000));
}

#[test]
fn nested_errors_are_the_innermost() {
    let error = layout(
        &i686(),
        &Shape::Struct(vec![
            s(Scalar::Bit8),
            Shape::Array(Box::new(s(Scalar::Bit64)), 0x1000_0000),
        ]),
    )
    .expect_err("too large");
    assert_eq!(error.part, Part::Length);
    assert_eq!(error.size, 0x8000_0000);
}
