//! The one layout authority: the C layout of every Lilac type on a target.
//!
//! An `object` is laid out exactly as the C struct with the same fields in
//! the same order, so `sizeof`, `_Alignof` and every `offsetof` agree with
//! the target's C compiler: each field at the next multiple of its
//! alignment, the size padded to a multiple of the largest alignment.
//! Fields are never reordered. A struct with no fields has size 0 and
//! alignment 1, as GCC and Clang lay out an empty struct. An array is a C
//! array: its elements in a row, each the element's (padded) size apart.
//!
//! A tagged union ([`tagged_layout`]) is the C
//! `struct { uintN_t tag; union { P0 p0; P1 p1; … } payload; }`.
//!
//! Every size is checked against the target's limit
//! ([`Target::max_object_size`]): a layout past it is [`TooLarge`], never a
//! wrapped size. Scalars come from [`Target::scalar_layout`], the one table
//! of C ABI facts. The functions compose: callers lay out the parts first
//! (by their [`SizeAlign`]) and then the aggregate, so no type is laid out
//! twice. [`layout`] does the same over a whole [`Shape`] tree.

use std::fmt;

use lilac_target::{Scalar, Target};

/// The size and alignment of a laid-out type, in bytes. The alignment is a
/// power of two, and the size a multiple of it.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct SizeAlign {
    /// `sizeof`.
    pub size: u64,
    /// `_Alignof`.
    pub align: u64,
}

impl SizeAlign {
    /// A scalar's size and alignment on `target`.
    #[must_use]
    pub fn scalar(target: &Target, scalar: Scalar) -> Self {
        let (size, align) = target.scalar_layout(scalar);
        Self { size, align }
    }
}

/// An aggregate's layout: its size, its alignment and the offset of each
/// part (a struct's fields in order; a tagged union's tag, then each
/// payload).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Layout {
    /// `sizeof`.
    pub size: u64,
    /// `_Alignof`.
    pub align: u64,
    /// `offsetof` of each part.
    pub fields: Vec<u64>,
}

impl Layout {
    /// The size and alignment, without the offsets.
    #[must_use]
    pub fn size_align(&self) -> SizeAlign {
        SizeAlign {
            size: self.size,
            align: self.align,
        }
    }
}

/// The part of an aggregate whose placement crossed the target's limit.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum Part {
    /// The part at this index of [`Layout::fields`]: a struct's field (the
    /// last one when only the trailing padding crosses), or a tagged
    /// union's tag (0) or payload (`1 + index`).
    Field(usize),
    /// An array's length: the elements together cross it.
    Length,
}

/// A layout larger than the target allows.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct TooLarge {
    /// Where it crossed the limit.
    pub part: Part,
    /// The size in bytes the type reached there (exact: it may not fit a
    /// `u64`).
    pub size: u128,
    /// The target's limit ([`Target::max_object_size`]).
    pub limit: u64,
}

impl fmt::Display for TooLarge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} bytes is larger than the target's limit of {} bytes",
            self.size, self.limit
        )
    }
}

/// A type to lay out, as a tree. The compiler composes [`struct_layout`],
/// [`array_layout`] and [`tagged_layout`] directly; this is the same over a
/// whole type at once.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Shape {
    /// One scalar.
    Scalar(Scalar),
    /// `len` elements in a row (a C array).
    Array(Box<Shape>, u64),
    /// A C struct of these fields, in order.
    Struct(Vec<Shape>),
    /// A tagged union: the C
    /// `struct { tag; union { payloads… } payload; }` (no union when there
    /// are no payloads).
    Tagged {
        /// The tag's integer container.
        tag: Scalar,
        /// One per variant that has a payload.
        payloads: Vec<Shape>,
    },
}

/// Lays out `shape` on `target`. An error is that of the innermost shape
/// that crossed the limit.
pub fn layout(target: &Target, shape: &Shape) -> Result<Layout, TooLarge> {
    match shape {
        Shape::Scalar(scalar) => {
            let SizeAlign { size, align } = SizeAlign::scalar(target, *scalar);
            Ok(Layout {
                size,
                align,
                fields: Vec::new(),
            })
        }
        Shape::Array(elem, len) => {
            let elem = layout(target, elem)?.size_align();
            let SizeAlign { size, align } = array_layout(target, elem, *len)?;
            Ok(Layout {
                size,
                align,
                fields: Vec::new(),
            })
        }
        Shape::Struct(fields) => {
            let fields = parts(*target, fields)?;
            struct_layout(target, &fields)
        }
        Shape::Tagged { tag, payloads } => {
            let payloads = parts(*target, payloads)?;
            tagged_layout(target, *tag, &payloads)
        }
    }
}

/// The size and alignment of each of `shapes`.
fn parts(target: Target, shapes: &[Shape]) -> Result<Vec<SizeAlign>, TooLarge> {
    shapes
        .iter()
        .map(|shape| layout(&target, shape).map(|layout| layout.size_align()))
        .collect()
}

/// The C struct of `fields`, in order: each at the next multiple of its
/// alignment, the size padded to the largest alignment (at least 1). No
/// fields is size 0, alignment 1.
pub fn struct_layout(target: &Target, fields: &[SizeAlign]) -> Result<Layout, TooLarge> {
    let limit = target.max_object_size();
    let mut offsets = Vec::with_capacity(fields.len());
    let mut end: u128 = 0;
    let mut align: u64 = 1;
    for (index, field) in fields.iter().enumerate() {
        let offset = align_up(end, field.align);
        end = offset + u128::from(field.size);
        check(end, limit, Part::Field(index))?;
        offsets.push(narrow(offset));
        align = align.max(field.align);
    }
    let size = align_up(end, align);
    check(size, limit, Part::Field(fields.len().saturating_sub(1)))?;
    Ok(Layout {
        size: narrow(size),
        align,
        fields: offsets,
    })
}

/// The C array of `len` elements: the element's size `len` times, aligned
/// as the element.
pub fn array_layout(target: &Target, elem: SizeAlign, len: u64) -> Result<SizeAlign, TooLarge> {
    let size = u128::from(elem.size) * u128::from(len);
    check(size, target.max_object_size(), Part::Length)?;
    Ok(SizeAlign {
        size: narrow(size),
        align: elem.align,
    })
}

/// The tagged union of a `tag` and `payloads`: the C
/// `struct { uintN_t tag; union { P0 p0; P1 p1; … } payload; }`. The tag is
/// at 0; the payload area at the tag's size rounded up to the strictest
/// payload alignment, where every payload starts; the size is that offset
/// plus the largest payload, rounded up to the overall alignment. With no
/// payloads it is just the tag. The offsets are the tag's, then each
/// payload's.
pub fn tagged_layout(
    target: &Target,
    tag: Scalar,
    payloads: &[SizeAlign],
) -> Result<Layout, TooLarge> {
    let tag = SizeAlign::scalar(target, tag);
    let Some(largest) = payloads
        .iter()
        .enumerate()
        .max_by(|(a_index, a), (b_index, b)| a.size.cmp(&b.size).then(b_index.cmp(a_index)))
        .map(|(index, _)| index)
    else {
        return Ok(Layout {
            size: tag.size,
            align: tag.align,
            fields: vec![0],
        });
    };
    let limit = target.max_object_size();
    let payload_align = payloads
        .iter()
        .map(|payload| payload.align)
        .max()
        .unwrap_or(1);
    let area = align_up(u128::from(tag.size), payload_align);
    let end = area + u128::from(payloads[largest].size);
    check(end, limit, Part::Field(1 + largest))?;
    let align = tag.align.max(payload_align);
    let size = align_up(end, align);
    check(size, limit, Part::Field(1 + largest))?;
    let area = narrow(area);
    let mut fields = vec![0];
    fields.extend(payloads.iter().map(|_| area));
    Ok(Layout {
        size: narrow(size),
        align,
        fields,
    })
}

/// `value` rounded up to a multiple of `align` (a power of two).
fn align_up(value: u128, align: u64) -> u128 {
    debug_assert!(align.is_power_of_two(), "alignment {align}");
    let align = u128::from(align.max(1));
    value.div_ceil(align) * align
}

/// `Err` when `size` is past `limit`.
fn check(size: u128, limit: u64, part: Part) -> Result<(), TooLarge> {
    if size > u128::from(limit) {
        Err(TooLarge { part, size, limit })
    } else {
        Ok(())
    }
}

/// A size already checked against a `u64` limit.
fn narrow(value: u128) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests;
