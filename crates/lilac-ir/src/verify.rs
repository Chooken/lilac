//! Checking a module: the verifier, and the checks of what stays live
//! after dead-code elimination (`@compile_error`, `@syscall`).

use std::collections::{HashMap, HashSet};

use lilac_error::{codes, Diagnostic, Diags, ErrorGuaranteed};
use lilac_span::Span;

use crate::print::print_op;
use crate::{
    builtin_layout, builtin_sig, fields_of, is_variadic_arg_type, trace, ArithOp, AtomicOrdering,
    CallGraph, LirBlock, LirCallee, LirConst, LirFunc, LirInst, LirModule, LirOperand, LirTerm,
    LirTy, PlaceName, Reg, Symbol, LILAC_IR_VERSION,
};

/// Reports every `@compile_error` site in `module` as `E031`. Run after DCE:
/// a site is an error exactly when its function is reachable. The diagnostic
/// labels both the message literal and the call, and traces every live call
/// site that reaches it ([`CallGraph::trace`], names from `names`).
///
/// # Errors
///
/// Returns [`ErrorGuaranteed`] when at least one site was reported.
pub fn check_live_compile_errors<S: std::hash::BuildHasher>(
    module: &LirModule,
    names: &HashMap<String, String, S>,
    diags: &mut Diags,
) -> Result<(), ErrorGuaranteed> {
    let graph = CallGraph::new(module);
    let mut failed = false;
    for func in &module.funcs {
        for inst in func.blocks.iter().flat_map(|block| &block.insts) {
            if let LirInst::CompileError {
                msg,
                msg_span,
                call_span,
            } = inst
            {
                failed = true;
                diags.emit(
                    Diagnostic::error(codes::E031, msg.clone())
                        .label(*msg_span, true, "message here".to_owned())
                        .label(*call_span, false, "evaluated here".to_owned())
                        .trace(graph.trace(func.name.as_str(), *call_span, names)),
                );
            }
        }
    }
    if failed {
        Err(diags.already_reported())
    } else {
        Ok(())
    }
}

/// `E035` for each `@syscall` a reachable user function makes when the
/// target has no system-call instruction (`has_syscall` is false). The
/// runtime's own `.lilac.*` calls are the compiler's business, not the user's.
///
/// # Errors
///
/// Returns `Err` after emitting a diagnostic for every such call.
pub fn check_live_syscalls<S: std::hash::BuildHasher>(
    module: &LirModule,
    has_syscall: bool,
    names: &HashMap<String, String, S>,
    diags: &mut Diags,
) -> Result<(), ErrorGuaranteed> {
    if has_syscall {
        return Ok(());
    }
    let graph = CallGraph::new(module);
    let mut failed = false;
    for func in module
        .funcs
        .iter()
        .filter(|func| !trace::is_hidden(func.name.as_str()))
    {
        for inst in func.blocks.iter().flat_map(|block| &block.insts) {
            let LirInst::Call {
                func: LirCallee::Builtin(name),
                span,
                ..
            } = inst
            else {
                continue;
            };
            if name != "@syscall" {
                continue;
            }
            failed = true;
            diags.emit(
                Diagnostic::error(
                    codes::E035,
                    format!("`@syscall` is not available on `{}`", module.target),
                )
                .label(*span, true, "no system-call instruction on this target".to_owned())
                .help("`@syscall` needs Linux on x86-64 or aarch64; choose another arm with `@builtin.Target`")
                .trace(graph.trace(func.name.as_str(), *span, names)),
            );
        }
    }
    if failed {
        Err(diags.already_reported())
    } else {
        Ok(())
    }
}

/// Verifies a module: every rule failure is a spanned `E070` diagnostic.
/// A `CompileError` node reaching here means the live `@compile_error` check
/// never ran — an internal error (`bug!`), never a user diagnostic.
pub fn verify(module: &LirModule, diags: &mut Diags) -> Result<(), ErrorGuaranteed> {
    let mut verifier = Verifier {
        module,
        diags,
        failed: false,
    };
    verifier.run();
    if verifier.failed {
        Err(verifier.diags.already_reported())
    } else {
        Ok(())
    }
}

pub(crate) struct Verifier<'a> {
    pub(crate) module: &'a LirModule,
    pub(crate) diags: &'a mut Diags,
    pub(crate) failed: bool,
}

/// One register definition: whether it holds a constant (constant branches
/// are an error), and
/// its declared type (so operand widths can be checked).
#[derive(Clone)]
pub(crate) struct RegDef<'a> {
    is_const: bool,
    ty: &'a LirTy,
}

/// Declared places of the function being verified, with their types.
type Places<'a> = HashMap<&'a str, &'a LirTy>;
/// Defined registers of the block being verified.
type Regs<'a> = HashMap<&'a str, RegDef<'a>>;

impl Verifier<'_> {
    fn error(&mut self, msg: String, span: Span) {
        self.failed = true;
        self.diags.emit(Diagnostic::error(codes::E070, msg).label(
            span,
            true,
            "invalid IR here".to_owned(),
        ));
    }

    fn run(&mut self) {
        if self.module.version != LILAC_IR_VERSION {
            self.error(
                format!(
                    "unsupported IR version `{}` (want `{LILAC_IR_VERSION}`)",
                    self.module.version
                ),
                Span::dummy(),
            );
            return;
        }
        let mut seen: HashSet<&str> = HashSet::new();
        for func in &self.module.funcs {
            if !seen.insert(func.name.as_str()) {
                self.error(format!("duplicate function `{}`", func.name), Span::dummy());
            }
            self.verify_func(func);
        }
    }

    /// Whether a type is known: a scalar, a pointer to a known type or to
    /// `#unknown` (the opaque `void*`; bare `#unknown` has no size), or a
    /// builtin or module layout.
    pub(crate) fn valid_type(&self, ty: &LirTy) -> bool {
        match ty {
            LirTy::Bit(_) | LirTy::BitNative | LirTy::FBit(_) => true,
            LirTy::Ref(inner) => inner.name() == Some("unknown") || self.valid_type(inner),
            LirTy::Array(elem, _) => self.valid_type(elem),
            LirTy::Named(name) => {
                builtin_layout(ty).is_some() || self.module.layouts.contains_key(name)
            }
        }
    }

    fn verify_func<'a>(&mut self, func: &'a LirFunc) {
        if func.variadic && !func.is_extern {
            self.error(
                format!("only an extern may be variadic, but `{}` is", func.name),
                Span::dummy(),
            );
        }
        if func.is_extern {
            if !func.blocks.is_empty() {
                self.error(
                    format!("extern `{}` must have no body", func.name),
                    Span::dummy(),
                );
            }
            let mut places: Places<'a> = HashMap::new();
            self.verify_signature(func, &mut places);
            return;
        }
        if func.blocks.is_empty() {
            self.error(
                format!("function `{}` has no blocks", func.name),
                Span::dummy(),
            );
            return;
        }
        let mut places: Places<'a> = HashMap::new();
        self.verify_signature(func, &mut places);
        let mut assigned: HashSet<&str> = HashSet::new();
        let last = func.blocks.len().saturating_sub(1);
        for (index, block) in func.blocks.iter().enumerate() {
            let has_fallthrough = index < last;
            self.verify_block(func, block, &mut places, &mut assigned, has_fallthrough);
        }
        for (place, _) in &func.outs {
            if !assigned.contains(place.as_str()) {
                self.error(
                    format!("out place `{place}` of `{}` is never assigned", func.name),
                    Span::dummy(),
                );
            }
        }
    }

    /// Validates the signature places shared by functions and externs: no
    /// duplicate place names and every `in`/`out` type known.
    fn verify_signature<'a>(&mut self, func: &'a LirFunc, places: &mut Places<'a>) {
        for (place, ty) in func.ins.iter().chain(func.outs.iter()) {
            if places.insert(place.as_str(), ty).is_some() {
                self.error(
                    format!("duplicate place `{place}` in `{}`", func.name),
                    Span::dummy(),
                );
            }
            if !self.valid_type(ty) {
                self.error(format!("unknown type `{ty}`"), Span::dummy());
            }
        }
    }

    fn verify_block<'a>(
        &mut self,
        func: &'a LirFunc,
        block: &'a LirBlock,
        places: &mut Places<'a>,
        assigned: &mut HashSet<&'a str>,
        has_fallthrough: bool,
    ) {
        let mut regs: Regs = HashMap::new();
        for inst in &block.insts {
            self.verify_inst(func, inst, places, &mut regs, assigned);
        }
        self.verify_term(func, &block.term, &regs, has_fallthrough);
    }

    #[allow(clippy::too_many_lines)]
    fn verify_inst<'a>(
        &mut self,
        func: &LirFunc,
        inst: &'a LirInst,
        places: &mut Places<'a>,
        regs: &mut Regs<'a>,
        assigned: &mut HashSet<&'a str>,
    ) {
        match inst {
            LirInst::CopyReg { src, dst, ty, span } => {
                self.check_operand(src, regs, places, *span, func);
                self.check_type(ty, *span);
                self.define_reg(regs, dst, ty, false, *span, func);
            }
            LirInst::CopyPlace { src, dst, span } => {
                self.check_operand(src, regs, places, *span, func);
                self.check_copy_place(src, dst, places, regs, *span, func);
                assigned.insert(dst.as_str());
            }
            LirInst::Const {
                dst,
                value,
                ty,
                span,
            } => {
                self.check_const(value, ty, *span);
                self.check_type(ty, *span);
                self.define_reg(regs, dst, ty, true, *span, func);
            }
            LirInst::Load { .. } | LirInst::Addr { .. } => {
                self.verify_mem_op(func, inst, places, regs);
            }
            LirInst::AddrFunc {
                func: target,
                dst,
                ty,
                span,
            } => {
                self.check_addr_func(target, ty, *span);
                self.define_reg(regs, dst, ty, false, *span, func);
            }
            LirInst::AddrGlobal {
                global,
                dst,
                ty,
                span,
            } => {
                self.check_addr_global(global, ty, *span);
                self.define_reg(regs, dst, ty, false, *span, func);
            }
            LirInst::Call {
                func: callee,
                ins,
                outs,
                span,
            } => {
                self.check_call_target(callee, ins, outs, places, *span, regs);
                self.track_call_places(places, assigned, ins, outs, *span);
            }
            LirInst::Field {
                obj,
                name,
                idx,
                dst,
                ty,
                span,
            } => {
                match places.get(obj.as_str()).copied() {
                    Some(owner_ty) => self.check_field_layout(owner_ty, name, *idx, ty, *span),
                    None => self.error(format!("field of undeclared place `{obj}`"), *span),
                }
                self.define_reg(regs, dst, ty, false, *span, func);
            }
            LirInst::Alloca { ty, place, span } => {
                self.check_type(ty, *span);
                if places.insert(place.as_str(), ty).is_some() {
                    self.error(
                        format!("duplicate place `{place}` in `{}`", func.name),
                        *span,
                    );
                }
            }
            LirInst::Drop { place, span } => {
                if !places.contains_key(place.as_str()) {
                    self.error(
                        format!("drop of undeclared place `{place}` in `{}`", func.name),
                        *span,
                    );
                }
            }
            LirInst::Arith {
                op,
                lhs,
                rhs,
                dst,
                ty,
                span,
            } => {
                for reg in [lhs, rhs] {
                    if !regs.contains_key(reg.as_str()) {
                        self.error(format!("use of undefined register `{reg}`"), *span);
                    }
                }
                self.check_arith_width(op, ty, lhs, rhs, regs, *span);
                self.define_reg(regs, dst, ty, false, *span, func);
            }
            LirInst::CompileError { call_span, .. } => {
                lilac_error::bug!(*call_span, "`@compile_error` reached verify");
            }
            LirInst::Zero { place, span } => {
                if !places.contains_key(place.as_str()) {
                    self.error(
                        format!("zero of undeclared place `{place}` in `{}`", func.name),
                        *span,
                    );
                }
                assigned.insert(place.as_str());
            }
            LirInst::StoreField { .. }
            | LirInst::Store { .. }
            | LirInst::Convert { .. }
            | LirInst::FieldAddr { .. } => {
                self.verify_store_ops(func, inst, places, regs);
            }
            LirInst::AtomicLoad { .. }
            | LirInst::AtomicStore { .. }
            | LirInst::AtomicRmw { .. }
            | LirInst::CmpXchg { .. }
            | LirInst::Fence { .. } => self.verify_atomic(func, inst, regs),
            LirInst::ElemAddr {
                array,
                index,
                dst,
                ty,
                span,
            } => {
                self.verify_elem_addr(array, index, ty, *span, regs);
                self.check_type(ty, *span);
                self.define_reg(regs, dst, ty, false, *span, func);
            }
        }
    }

    /// Checks `elem.addr`: `&array` is a `ref [N x T]`, `&index` a
    /// `#bit_native`, and the result `ref T`.
    fn verify_elem_addr(&mut self, array: &Reg, index: &Reg, ty: &LirTy, span: Span, regs: &Regs) {
        match regs.get(array.as_str()).map(|def| def.ty) {
            None => self.error(format!("use of undefined register `{array}`"), span),
            Some(LirTy::Ref(inner)) if matches!(**inner, LirTy::Array(..)) => {
                if let LirTy::Array(elem, _) = &**inner {
                    if !ty.is_ref() || **elem != *ty.strip_ref() {
                        self.error(
                            format!("`elem.addr` of `{inner}` must be `ref {elem}`, found `{ty}`"),
                            span,
                        );
                    }
                }
            }
            Some(found) => self.error(
                format!("`elem.addr` array `{array}` must be a `ref` array, found `{found}`"),
                span,
            ),
        }
        match regs.get(index.as_str()).map(|def| def.ty) {
            None => self.error(format!("use of undefined register `{index}`"), span),
            Some(LirTy::BitNative) => {}
            Some(found) => self.error(
                format!("`elem.addr` index `{index}` must be `#bit_native`, found `{found}`"),
                span,
            ),
        }
    }

    /// Verifies an atomic instruction: an integer type, a `ref` of that type
    /// as the address, operands of that type, and an ordering the
    /// instruction allows.
    fn verify_atomic<'a>(&mut self, func: &LirFunc, inst: &'a LirInst, regs: &mut Regs<'a>) {
        let (name, ptr, operands, dst, ty, order, span) = match inst {
            LirInst::AtomicLoad {
                ptr,
                dst,
                ty,
                order,
                span,
            } => ("atomic.load", ptr, Vec::new(), Some(dst), ty, *order, *span),
            LirInst::AtomicStore {
                value,
                ptr,
                ty,
                order,
                span,
            } => ("atomic.store", ptr, vec![value], None, ty, *order, *span),
            LirInst::AtomicRmw {
                ptr,
                value,
                dst,
                ty,
                order,
                span,
                ..
            } => ("atomic.rmw", ptr, vec![value], Some(dst), ty, *order, *span),
            LirInst::CmpXchg {
                ptr,
                expected,
                desired,
                dst,
                ty,
                order,
                span,
            } => (
                "atomic.cmpxchg",
                ptr,
                vec![expected, desired],
                Some(dst),
                ty,
                *order,
                *span,
            ),
            LirInst::Fence { order, span } => {
                if *order == AtomicOrdering::Relaxed {
                    self.error("`fence` cannot be `relaxed`".to_owned(), *span);
                }
                return;
            }
            _ => lilac_error::bug!(Span::dummy(), "non-atomic instruction in verify_atomic"),
        };
        let bad_order = match inst {
            LirInst::AtomicLoad { .. } => {
                matches!(order, AtomicOrdering::Release | AtomicOrdering::AcqRel)
            }
            LirInst::AtomicStore { .. } => {
                matches!(order, AtomicOrdering::Acquire | AtomicOrdering::AcqRel)
            }
            _ => false,
        };
        if bad_order {
            self.error(format!("`{name}` cannot be `{}`", order.name()), span);
        }
        if !ty.is_int() {
            self.error(
                format!("`{name}` needs a bit-container type, found `{ty}`"),
                span,
            );
        }
        match regs.get(ptr.as_str()).map(|def| def.ty) {
            None => self.error(format!("use of undefined register `{ptr}`"), span),
            Some(found) if !found.is_ref() || found.strip_ref() != ty => self.error(
                format!("`{name}` address `{ptr}` must be `ref {ty}`, found `{found}`"),
                span,
            ),
            Some(_) => {}
        }
        for reg in operands {
            match regs.get(reg.as_str()).map(|def| def.ty) {
                None => self.error(format!("use of undefined register `{reg}`"), span),
                Some(found) if found != ty => self.error(
                    format!("`{name}` operand `{reg}` must be `{ty}`, found `{found}`"),
                    span,
                ),
                Some(_) => {}
            }
        }
        if let Some(dst) = dst {
            self.define_reg(regs, dst, ty, false, span, func);
        }
    }

    /// Checks a place copy: the destination must be declared and the source
    /// operand's type must be compatible. Two reference places are compatible
    /// under opaque pointers (`ref X` -> `ref Y`), but a `ref`-to-value copy or
    /// two distinct value types are rejected, so an adapter such as a
    /// dereference cannot slip through as a raw pointer copy.
    fn check_copy_place(
        &mut self,
        src: &LirOperand,
        dst: &PlaceName,
        places: &Places,
        regs: &Regs,
        span: Span,
        func: &LirFunc,
    ) {
        let Some(dst_ty) = places.get(dst.as_str()).copied() else {
            self.error(
                format!("copy to undeclared place `{dst}` in `{}`", func.name),
                span,
            );
            return;
        };
        let src_ty = match src {
            LirOperand::Reg(reg) => regs.get(reg.as_str()).map(|def| def.ty),
            LirOperand::Place(place) => places.get(place.as_str()).copied(),
        };
        // Under opaque pointers any two reference places are layout-compatible,
        // so `ref X` -> `ref Y` is a pointer copy. A reference/value mix or two
        // distinct value types is still ill-typed.
        if let Some(src_ty) = src_ty.filter(|src_ty| {
            src_ty.is_ref() != dst_ty.is_ref() || (!src_ty.is_ref() && *src_ty != dst_ty)
        }) {
            self.error(
                format!(
                    "copy type mismatch in `{}`: `{src_ty}` -> `{dst_ty}`",
                    func.name
                ),
                span,
            );
        }
    }

    /// Checks a `const` against its type: an integer must fit its bit
    /// container (or be the null reference), a float needs a float type,
    /// and data must be declared and taken as a buffer or a byte address.
    fn check_const(&mut self, value: &LirConst, ty: &LirTy, span: Span) {
        let fits = match (value, ty) {
            (LirConst::Int(value), LirTy::Bit(bits)) => {
                *bits >= 64 || *value >> u32::from(*bits) == 0
            }
            // `#bit_native`'s width is the target's; lowering sizes it.
            (LirConst::Int(_), LirTy::BitNative) | (LirConst::Float(_), LirTy::FBit(_)) => true,
            (LirConst::Int(value), LirTy::Ref(_)) => *value == 0,
            (LirConst::Data(name), _) => {
                self.check_data_const(name, ty, span);
                true
            }
            _ => false,
        };
        if !fits {
            self.error(format!("`const {value}` cannot be a `{ty}`"), span);
        }
    }

    /// Checks a data `const`: declared data, taken as a `#byte_buffer`,
    /// `#format_args` or `ref #bit8`.
    fn check_data_const(&mut self, name: &Symbol, ty: &LirTy, span: Span) {
        if !self.module.data.iter().any(|datum| datum.name == *name) {
            self.error(
                format!("const of undeclared data `!{}`", name.as_str()),
                span,
            );
        }
        if *ty != LirTy::byte_buffer() && *ty != LirTy::format_args() && *ty != LirTy::byte_ref() {
            self.error(
                format!(
                    "data-symbol const needs `#byte_buffer`, `#format_args`, or `ref #bit8`, found `{ty}`"
                ),
                span,
            );
        }
    }

    /// Verifies stores and conversions (field stores, deref stores,
    /// width conversions, field addresses).
    fn verify_store_ops<'a>(
        &mut self,
        func: &LirFunc,
        inst: &'a LirInst,
        places: &Places,
        regs: &mut Regs<'a>,
    ) {
        match inst {
            LirInst::StoreField {
                value,
                obj,
                name,
                idx,
                span,
            } => {
                self.check_operand(value, regs, places, *span, func);
                self.check_field_ref(obj, name, *idx, *span, places);
            }
            LirInst::Store { src, dst, span } => {
                self.check_operand(src, regs, places, *span, func);
                match places.get(dst.as_str()).copied() {
                    Some(ty) if ty.is_ref() => {}
                    _ => {
                        self.error(format!("store through non-reference place `{dst}`"), *span);
                    }
                }
            }
            LirInst::Convert {
                src, dst, ty, span, ..
            } => {
                if !regs.contains_key(src.as_str()) {
                    self.error(format!("use of undefined register `{src}`"), *span);
                }
                self.check_type(ty, *span);
                self.define_reg(regs, dst, ty, false, *span, func);
            }
            LirInst::FieldAddr {
                obj,
                name,
                idx,
                dst,
                ty,
                span,
            } => {
                self.check_field_ref(obj, name, *idx, *span, places);
                self.check_type(ty, *span);
                self.define_reg(regs, dst, ty, false, *span, func);
            }
            _ => {
                lilac_error::bug!(Span::dummy(), "non-store instruction in store verifier");
            }
        }
    }

    /// Tracks call places (declared) and marks outputs assigned.
    fn track_call_places<'a>(
        &mut self,
        places: &Places,
        assigned: &mut HashSet<&'a str>,
        ins: &'a [PlaceName],
        outs: &'a [PlaceName],
        span: Span,
    ) {
        for place in ins.iter().chain(outs.iter()) {
            if !places.contains_key(place.as_str()) {
                self.error(format!("call through undeclared place `{place}`"), span);
            }
        }
        for place in outs {
            assigned.insert(place.as_str());
        }
    }

    /// Checks a field reference against its owner's layout (name and index).
    fn check_field_ref(
        &mut self,
        obj: &PlaceName,
        name: &str,
        idx: u32,
        span: Span,
        places: &Places,
    ) {
        let Some(owner_ty) = places.get(obj.as_str()).copied() else {
            self.error(format!("field of undeclared place `{obj}`"), span);
            return;
        };
        let owner_ty = owner_ty.strip_ref();
        let Some(layout) = fields_of(&self.module.layouts, owner_ty) else {
            self.error(
                format!("field of `{obj}` with unknown layout `{owner_ty}`"),
                span,
            );
            return;
        };
        let Some((field_name, _)) = layout.get(idx as usize) else {
            self.error(
                format!("field index `{idx}` out of bounds for `{owner_ty}`"),
                span,
            );
            return;
        };
        if field_name != name {
            self.error(
                format!("field `.{name}` is not at index `{idx}` of `{owner_ty}`"),
                span,
            );
        }
    }

    /// Checks `addr.func` (target exists, type known).
    fn check_addr_func(&mut self, target: &Symbol, ty: &LirTy, span: Span) {
        if !self.module.funcs.iter().any(|item| item.name == *target) {
            self.error(format!("address of unknown function `{target}`"), span);
        }
        self.check_type(ty, span);
    }

    /// Checks `addr.global`: the global is declared and the result is a
    /// reference to its type (opaque for a `ref` global).
    fn check_addr_global(&mut self, global: &Symbol, ty: &LirTy, span: Span) {
        let Some(declared) = self.module.globals.iter().find(|item| item.name == *global) else {
            self.error(
                format!("address of unknown global `!{}`", global.as_str()),
                span,
            );
            return;
        };
        let fits = *ty == declared.ty.clone().to_ref()
            || (matches!(declared.ty, LirTy::Ref(_)) && *ty == LirTy::opaque_ref());
        if !fits {
            self.error(
                format!(
                    "`addr.global !{}` needs `ref {}`, found `{ty}`",
                    global.as_str(),
                    declared.ty
                ),
                span,
            );
        }
        self.check_type(ty, span);
    }

    /// Checks `load`/`addr` (place exists, type known) and defines the reg.
    fn verify_mem_op<'a>(
        &mut self,
        func: &LirFunc,
        inst: &'a LirInst,
        places: &Places,
        regs: &mut Regs<'a>,
    ) {
        let (place, dst, ty, span) = match inst {
            LirInst::Load { src, dst, ty, span } => (src, dst, ty, span),
            LirInst::Addr {
                place,
                dst,
                ty,
                span,
            } => (place, dst, ty, span),
            _ => lilac_error::bug!(Span::dummy(), "non-memory instruction in verify_mem_op"),
        };
        if !places.contains_key(place.as_str()) {
            self.error(
                format!("address of undeclared place `{place}` in `{}`", func.name),
                *span,
            );
        }
        self.check_type(ty, *span);
        self.define_reg(regs, dst, ty, false, *span, func);
    }

    /// Checks a call target: register definedness for indirect calls,
    /// name and arity for direct calls, signature for builtin calls.
    fn check_call_target(
        &mut self,
        callee: &LirCallee,
        ins: &[PlaceName],
        outs: &[PlaceName],
        places: &Places,
        span: Span,
        regs: &Regs,
    ) {
        match callee {
            LirCallee::Reg(reg) => {
                if !regs.contains_key(reg.as_str()) {
                    self.error(format!("call through undefined register `{reg}`"), span);
                }
            }
            LirCallee::Func(target) => {
                let Some(found) = self.module.funcs.iter().find(|func| func.name == *target) else {
                    self.error(format!("call to unknown function `{target}`"), span);
                    return;
                };
                let ins_fit = if found.variadic {
                    ins.len() >= found.ins.len()
                } else {
                    ins.len() == found.ins.len()
                };
                if !ins_fit || outs.len() != found.outs.len() {
                    let at_least = if found.variadic { "at least " } else { "" };
                    self.error(
                        format!(
                            "call to `{target}` takes {at_least}{} in and {} out, found {} in and {} out",
                            found.ins.len(),
                            found.outs.len(),
                            ins.len(),
                            outs.len(),
                        ),
                        span,
                    );
                    return;
                }
                for place in &ins[found.ins.len()..] {
                    if let Some(ty) = places.get(place.as_str()) {
                        if !is_variadic_arg_type(ty) {
                            self.error(
                                format!("variadic argument `{place}` to `{target}` has type `{ty}`, which C would promote"),
                                span,
                            );
                        }
                    }
                }
            }
            LirCallee::Builtin(name) => {
                let Some(sig) = builtin_sig(name) else {
                    self.error(format!("call to unknown builtin `{name}`"), span);
                    return;
                };
                if ins.len() != sig.ins.len() || outs.len() != sig.outs.len() {
                    self.error(
                        format!(
                            "call to `{name}` takes {} in and {} out, found {} in and {} out",
                            sig.ins.len(),
                            sig.outs.len(),
                            ins.len(),
                            outs.len(),
                        ),
                        span,
                    );
                    return;
                }
                let slots = ins
                    .iter()
                    .zip(&sig.ins)
                    .map(|slot| ("input", slot))
                    .chain(outs.iter().zip(&sig.outs).map(|slot| ("output", slot)));
                for (side, (place, expected)) in slots {
                    match places.get(place.as_str()) {
                        Some(actual) if *actual == expected => {}
                        actual => {
                            let actual =
                                actual.map_or("undeclared".to_owned(), ToString::to_string);
                            self.error(
                                format!(
                                    "builtin `{name}` {side} `{place}` is `{actual}`, expected `{expected}`"
                                ),
                                span,
                            );
                        }
                    }
                }
            }
        }
    }

    /// Checks a `field` against its owner's layout (name, index, and type).
    fn check_field_layout(
        &mut self,
        owner_ty: &LirTy,
        name: &str,
        idx: u32,
        ty: &LirTy,
        span: Span,
    ) {
        let owner_ty = owner_ty.strip_ref();
        let Some(layout) = fields_of(&self.module.layouts, owner_ty) else {
            self.error(format!("field with unknown layout `{owner_ty}`"), span);
            return;
        };
        let Some((field_name, field_ty)) = layout.get(idx as usize) else {
            self.error(
                format!("field index `{idx}` out of bounds for `{owner_ty}`"),
                span,
            );
            return;
        };
        if field_name != name || field_ty != ty {
            self.error(
                format!("field `.{name}` is `{field_ty}` at index `{idx}`, not `{ty}`"),
                span,
            );
        }
    }

    /// Checks arithmetic widths. Integer ops need bit-container operands and
    /// result; float ops need `#fbit16/32/64/128` operands (real float
    /// containers), with a `#bit8` result for float comparisons.
    fn check_arith_width(
        &mut self,
        op: &ArithOp,
        ty: &LirTy,
        lhs: &Reg,
        rhs: &Reg,
        regs: &Regs,
        span: Span,
    ) {
        let is_float_arith = matches!(
            op,
            ArithOp::FAdd | ArithOp::FSub | ArithOp::FMul | ArithOp::FDiv | ArithOp::FMod
        );
        let is_float_cmp = matches!(
            op,
            ArithOp::FLt | ArithOp::FGt | ArithOp::FLe | ArithOp::FGe
        );
        if is_float_cmp {
            if *ty != LirTy::Bit(8) {
                self.error(
                    format!("`{}` needs a `#bit8` result, found `{ty}`", print_op(op)),
                    span,
                );
            }
        } else if is_float_arith {
            if !ty.is_float() {
                self.error(
                    format!("`{}` needs a float type, found `{ty}`", print_op(op)),
                    span,
                );
                return;
            }
        } else if !ty.is_int() {
            self.error(
                format!(
                    "`{}` needs a bit-container type, found `{ty}`",
                    print_op(op)
                ),
                span,
            );
            return;
        }
        if !(is_float_arith || is_float_cmp) {
            // Integer operands must match each other, and the result unless
            // the op is a comparison (whose result is `#bit8`).
            let operand_tys: Vec<&LirTy> = [lhs, rhs]
                .iter()
                .filter_map(|reg| regs.get(reg.as_str()).map(|def| def.ty))
                .collect();
            if let [left, right] = operand_tys.as_slice() {
                let is_eq = matches!(op, ArithOp::Eq);
                let mismatched = left != right
                    || (!is_eq && !left.is_int())
                    || (!op.is_comparison() && *left != ty)
                    || (op.is_comparison() && *ty != LirTy::Bit(8));
                if mismatched {
                    self.error(
                        format!(
                            "`{}` operands `{left}`/`{right}` do not match result `{ty}`",
                            print_op(op)
                        ),
                        span,
                    );
                }
            }
            return;
        }
        for reg in [lhs, rhs] {
            if let Some(def) = regs.get(reg.as_str()) {
                if !def.ty.is_float() {
                    self.error(
                        format!(
                            "`{}` needs float operands, found `{}` in `{reg}`",
                            print_op(op),
                            def.ty
                        ),
                        span,
                    );
                }
            }
        }
    }

    fn check_operand(
        &mut self,
        operand: &LirOperand,
        regs: &Regs,
        places: &Places,
        span: Span,
        func: &LirFunc,
    ) {
        match operand {
            LirOperand::Reg(reg) => {
                if !regs.contains_key(reg.as_str()) {
                    self.error(
                        format!("use of undefined register `{reg}` in `{}`", func.name),
                        span,
                    );
                }
            }
            LirOperand::Place(place) => {
                if !places.contains_key(place.as_str()) {
                    self.error(
                        format!("use of undeclared place `{place}` in `{}`", func.name),
                        span,
                    );
                }
            }
        }
    }

    fn check_type(&mut self, ty: &LirTy, span: Span) {
        if !self.valid_type(ty) {
            self.error(format!("unknown type `{ty}`"), span);
        }
    }

    fn define_reg<'a>(
        &mut self,
        regs: &mut Regs<'a>,
        dst: &'a Reg,
        ty: &'a LirTy,
        is_const: bool,
        span: Span,
        func: &LirFunc,
    ) {
        if regs.insert(dst.as_str(), RegDef { is_const, ty }).is_some() {
            self.error(
                format!("register `{dst}` defined twice in `{}`", func.name),
                span,
            );
        }
    }

    fn verify_term(&mut self, func: &LirFunc, term: &LirTerm, regs: &Regs, has_fallthrough: bool) {
        let labels: HashSet<&str> = func
            .blocks
            .iter()
            .map(|block| block.label.as_str())
            .collect();
        match term {
            LirTerm::Branch { target, span } => {
                if !labels.contains(target.as_str()) {
                    self.error(
                        format!("branch to unknown block `{target}` in `{}`", func.name),
                        *span,
                    );
                }
            }
            LirTerm::BranchIf {
                cond,
                then,
                els,
                span,
            } => {
                match regs.get(cond.as_str()) {
                    None => {
                        self.error(format!("branch on undefined register `{cond}`"), *span);
                    }
                    Some(def) if def.is_const => {
                        self.error(
                            format!(
                                "branch on constant `{cond}`; lowering decides constant conditions"
                            ),
                            *span,
                        );
                    }
                    Some(_) => {}
                }
                for target in [then].into_iter().chain(els.as_ref()) {
                    if !labels.contains(target.as_str()) {
                        self.error(
                            format!("branch to unknown block `{target}` in `{}`", func.name),
                            *span,
                        );
                    }
                }
                if els.is_none() && !has_fallthrough {
                    self.error(
                        format!(
                            "`branch.if` without an `else` in `{}` has no following block for its false edge",
                            func.name
                        ),
                        *span,
                    );
                }
            }
            LirTerm::Return { .. } | LirTerm::Unreachable { .. } => {}
        }
    }
}
