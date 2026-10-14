//! Global variables: one `LirGlobal` per checked global, its initializer
//! folded to bytes in the target's layout, and the place a use names.

use lilac_hir::Layout;
use lilac_ir::{AddressTarget, LirAddress, LirGlobal, LirInst, LirTy, Symbol};
use lilac_resolve::DefId;
use lilac_span::Span;
use lilac_ty::TypeRef;

use crate::constant::Value;
use crate::symbols::namespace_prefix;
use crate::{BadDivision, FnState, Lower, Place};

impl Lower<'_> {
    /// Adds every global to the module. An initializer that is not a
    /// constant expression is recorded in `bad_globals` (`E038`), or in
    /// `bad_divisions` (`E060`) when a division in it panics, and the
    /// global starts zeroed.
    pub(crate) fn lower_globals(&mut self) {
        let mut globals: Vec<(&DefId, &lilac_hir::HirGlobal)> = self.hir.globals.iter().collect();
        globals.sort_by_key(|(def, _)| def.0);
        for (def, global) in globals {
            let ty = self.lir_ty(global.ty);
            let (init, addresses) = global
                .value
                .as_ref()
                .and_then(|value| match self.constant_or_panic(value) {
                    Ok(constant) => Some(self.encode_value(&constant, global.ty)),
                    // A division that panics is the reason: `E060`, not `E038`.
                    Err(Some(panic)) => {
                        self.bad_divisions.push(BadDivision {
                            func: None,
                            span: value.span,
                            panic,
                        });
                        None
                    }
                    Err(None) => {
                        self.bad_globals.push(value.span);
                        None
                    }
                })
                .unwrap_or_default();
            let name = self.global_symbol(*def);
            self.module.globals.push(LirGlobal {
                name,
                ty,
                init,
                addresses,
                span: global.span,
            });
        }
    }

    /// The symbol of global `def`: its qualified name, with a root global
    /// under `.root` (never a C symbol).
    pub(crate) fn global_symbol(&self, def: DefId) -> Symbol {
        let data = self.res.def(def);
        if data.parent.is_some() {
            return Symbol::new(self.res.qualified_name(def));
        }
        let path = self.res.namespace_path(data.ns);
        let prefix = match namespace_prefix(path, &data.name).as_str() {
            "" => ".root".to_owned(),
            prefix => prefix.to_owned(),
        };
        Symbol::new(format!("{prefix}.{}", data.name))
    }

    /// The place global `def` names: its address in a fresh `ref` place.
    pub(crate) fn global_place(
        &mut self,
        state: &mut FnState,
        def: DefId,
        ty: TypeRef,
        span: Span,
    ) -> Place {
        let pointer = if ty.is_ref {
            LirTy::opaque_ref()
        } else {
            self.lir_ty(ty).to_ref()
        };
        let reg = state.reg();
        state.emit(LirInst::AddrGlobal {
            global: self.global_symbol(def),
            dst: reg.clone(),
            ty: pointer.clone(),
            span,
        });
        let holder = state.tmp_lir(pointer, span);
        state.emit(LirInst::CopyPlace {
            src: lilac_ir::LirOperand::Reg(reg),
            dst: holder.clone(),
            span,
        });
        Place::Indirect(holder)
    }

    /// `value` as the initializer of a `ty` in the target's layout
    /// (little-endian): its runs, each nonzero scalar's bytes at its
    /// offset, in offset order, merged across gaps shorter than
    /// [`RUN_GAP`] that hold no address; and its addresses, in offset
    /// order. Nothing is allocated for zero bytes, so the work is
    /// proportional to the initialized scalars, not the object's size.
    /// Both are empty when every bit is zero.
    fn encode_value(&self, value: &Value, ty: TypeRef) -> (Vec<(u64, Vec<u8>)>, Vec<LirAddress>) {
        let mut scalars = Vec::new();
        let mut addresses = Vec::new();
        self.encode(value, ty, 0, &mut scalars, &mut addresses);
        scalars.sort_by_key(|(offset, _)| *offset);
        addresses.sort_by_key(|address| address.offset);
        let mut next_address = addresses.iter().map(|address| address.offset).peekable();
        let mut runs: Vec<(u64, Vec<u8>)> = Vec::new();
        for (offset, bytes) in scalars {
            if let Some((start, run)) = runs.last_mut() {
                let end = *start + run.len() as u64;
                if offset < end {
                    lilac_error::bug!(Span::dummy(), "two scalars of a global overlap");
                }
                // A run joined across an address would cover its slot.
                while next_address.next_if(|at| *at < end).is_some() {}
                let address_between = next_address.peek().is_some_and(|at| *at < offset);
                if offset - end < RUN_GAP && !address_between {
                    run.resize(usize::try_from(offset - *start).unwrap_or(usize::MAX), 0);
                    run.extend(bytes);
                    continue;
                }
            }
            runs.push((offset, bytes));
        }
        (runs, addresses)
    }

    /// Adds the bytes of `value` (of type `ty`, at offset `at`) to
    /// `scalars`, each nonzero scalar's at its offset in the type's module
    /// layout, and its addresses to `addresses`.
    fn encode(
        &self,
        value: &Value,
        ty: TypeRef,
        at: u64,
        scalars: &mut Vec<(u64, Vec<u8>)>,
        addresses: &mut Vec<LirAddress>,
    ) {
        match (value, self.layout(ty.id)) {
            (Value::Func(key), _) => addresses.push(LirAddress {
                offset: at,
                target: AddressTarget::Func(self.symbol(key)),
            }),
            (Value::Global(def), _) => addresses.push(LirAddress {
                offset: at,
                target: AddressTarget::Global(self.global_symbol(*def)),
            }),
            (Value::Int(bits, width), _) => {
                if *bits != 0 {
                    let len = (*width / 8) as usize;
                    scalars.push((at, bits.to_le_bytes().into_iter().take(len).collect()));
                }
            }
            (Value::Aggregate(values), Some(layout @ Layout::Object { .. })) => {
                let slots = layout.slots();
                let offsets = self.offsets(ty);
                for ((value, (_, ty)), offset) in values.iter().zip(slots).zip(offsets) {
                    self.encode(value, ty, at + offset, scalars, addresses);
                }
            }
            // The tag at its offset, then the live payload at the payload
            // area's (every payload field of a tagged layout has that
            // offset); the rest of the area stays zero.
            (Value::Variant(index, payload), Some(layout @ Layout::Enum { .. })) => {
                let slots = layout.slots();
                let offsets = self.offsets(ty);
                let tag = slots[0].1;
                let tag_width = match self.lir_ty(tag) {
                    LirTy::Bit(width) => u32::from(width),
                    _ => 8,
                };
                self.encode(
                    &Value::Int(u64::from(*index), tag_width),
                    tag,
                    at + offsets[0],
                    scalars,
                    addresses,
                );
                if let Some(payload) = payload {
                    let (field, _) = self.payload_field(ty.id, *index);
                    let field = field as usize;
                    if let (Some((_, ty)), Some(offset)) = (slots.get(field), offsets.get(field)) {
                        self.encode(payload, *ty, at + offset, scalars, addresses);
                    }
                }
            }
            // A zeroed array literal (`@array(T, N) {}`) leaves its zeros.
            _ => {}
        }
    }

    /// The byte offset of each slot of `ty`'s module layout.
    fn offsets(&self, ty: TypeRef) -> Vec<u64> {
        let name = self.type_name(ty.id);
        match self.module.layouts.get(&name) {
            Some(layout) => layout.fields.iter().map(|field| field.offset).collect(),
            None => lilac_error::bug!(Span::dummy(), "type `{name}` has no layout"),
        }
    }
}

/// Initializer runs closer than this many zero bytes are one run: the
/// padding between a global's fields does not split it, and a long zero
/// stretch (an array left zeroed) costs nothing.
const RUN_GAP: u64 = 16;
