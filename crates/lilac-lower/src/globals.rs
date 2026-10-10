//! Global variables: one `LirGlobal` per checked global, its initializer
//! folded to bytes in the target's layout, and the place a use names.

use lilac_hir::Layout;
use lilac_ir::{LirGlobal, LirInst, LirTy, Symbol};
use lilac_resolve::DefId;
use lilac_span::Span;
use lilac_ty::TypeRef;

use crate::constant::Value;
use crate::symbols::namespace_prefix;
use crate::{FnState, Lower, Place};

impl Lower<'_> {
    /// Adds every global to the module. An initializer that is not a
    /// constant expression is recorded in `bad_globals` (`E038`) and the
    /// global starts zeroed.
    pub(crate) fn lower_globals(&mut self) {
        let mut globals: Vec<(&DefId, &lilac_hir::HirGlobal)> = self.hir.globals.iter().collect();
        globals.sort_by_key(|(def, _)| def.0);
        for (def, global) in globals {
            let ty = self.lir_ty(global.ty);
            let init = global.value.as_ref().and_then(|value| {
                let constant = self.constant(value);
                if constant.is_none() {
                    self.bad_globals.push(value.span);
                }
                self.encode_value(&constant?, global.ty)
            });
            let name = self.global_symbol(*def);
            self.module.globals.push(LirGlobal {
                name,
                ty,
                init,
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

    /// `value` as the bytes of a `ty` in the target's layout (little-endian),
    /// or `None` when it is all zero bits.
    fn encode_value(&self, value: &Value, ty: TypeRef) -> Option<Vec<u8>> {
        let (size, _) = self.lir_size_align(&self.lir_ty(ty));
        let mut bytes = vec![0; usize::try_from(size).unwrap_or(0)];
        self.encode(value, ty, &mut bytes, 0);
        bytes.iter().any(|byte| *byte != 0).then_some(bytes)
    }

    /// Writes `value` (of type `ty`) into `bytes` at `at`.
    fn encode(&self, value: &Value, ty: TypeRef, bytes: &mut [u8], at: u64) {
        match (value, self.layout(ty.id)) {
            (Value::Int(bits, width), _) => {
                let len = (*width / 8) as usize;
                let start = usize::try_from(at).unwrap_or(usize::MAX);
                for (index, byte) in bits.to_le_bytes().iter().take(len).enumerate() {
                    if let Some(slot) = bytes.get_mut(start + index) {
                        *slot = *byte;
                    }
                }
            }
            (Value::Aggregate(values), Some(Layout::Object { fields })) => {
                let types: Vec<TypeRef> = fields.iter().map(|(_, ty)| *ty).collect();
                let offsets = self.offsets(&types);
                for ((value, ty), offset) in values.iter().zip(types).zip(offsets) {
                    self.encode(value, ty, bytes, at + offset);
                }
            }
            (Value::Variant(index, payload), Some(Layout::Enum { variants, tag })) => {
                let mut types = vec![TypeRef::value(*tag)];
                types.extend(variants.iter().filter_map(|(_, payload)| *payload));
                let offsets = self.offsets(&types);
                let tag_width = match self.lir_ty(types[0]) {
                    LirTy::Bit(width) => u32::from(width),
                    _ => 8,
                };
                self.encode(
                    &Value::Int(u64::from(*index), tag_width),
                    types[0],
                    bytes,
                    at + offsets[0],
                );
                if let Some(payload) = payload {
                    let field = variants
                        .iter()
                        .take(*index as usize)
                        .filter(|(_, payload)| payload.is_some())
                        .count()
                        + 1;
                    if let (Some(ty), Some(offset)) = (types.get(field), offsets.get(field)) {
                        self.encode(payload, *ty, bytes, at + offset);
                    }
                }
            }
            // A zeroed array literal (`@array(T, N) {}`) leaves its zeros.
            _ => {}
        }
    }

    /// The byte offset of each field of a C-layout struct of `types`.
    fn offsets(&self, types: &[TypeRef]) -> Vec<u64> {
        let mut offset: u64 = 0;
        types
            .iter()
            .map(|ty| {
                let (size, align) = self.lir_size_align(&self.lir_ty(*ty));
                let align = align.max(1);
                offset = offset.div_ceil(align) * align;
                let at = offset;
                offset += size;
                at
            })
            .collect()
    }

    fn lir_size_align(&self, ty: &LirTy) -> (u64, u64) {
        lilac_ir::size_align(&self.module.layouts, ty, self.ptr_bytes)
    }
}
