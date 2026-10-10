//! Reachability sweep: drops functions with no path from `main`, data
//! consts no surviving instruction names, and layouts no surviving type
//! names.
//!
//! The frontend lowers every declaration, so a function that nothing calls and a
//! `!data` const nothing references still survive into `verify`. This pass is
//! conservative and infallible: with no `main` it leaves the module untouched.
//! [`CallGraph`] keeps every edge, for the caller trees of `E031`, `E035`
//! and `E037`.

use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::BuildHasher;

use lilac_error::TraceFrame;
use lilac_span::Span;

use crate::trace::is_hidden;
use crate::{LirCallee, LirConst, LirFunc, LirInst, LirModule, LirTy, Symbol};

/// How a caller reaches a callee: a direct `call` or a taken `addr.func`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EdgeKind {
    /// `call @f`.
    Call,
    /// `addr.func @f`.
    Address,
}

/// One edge into a reached function: the caller, the instruction's span and
/// its kind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edge {
    /// Calling function.
    pub caller: Symbol,
    /// Span of the `call` / `addr.func`.
    pub span: Span,
    /// Edge kind.
    pub kind: EdgeKind,
}

/// The functions reachable from the roots.
#[derive(Clone, Debug, Default)]
pub struct Reach {
    reached: HashSet<Symbol>,
}

impl Reach {
    /// Whether `func` is reachable.
    #[must_use]
    pub fn contains(&self, func: &str) -> bool {
        self.reached.contains(func)
    }
}

/// Every `call` and `addr.func` edge between a module's functions, by
/// callee, in module order. [`Self::trace`] turns it into a diagnostic's
/// caller tree.
#[derive(Clone, Debug, Default)]
pub struct CallGraph {
    callers: HashMap<Symbol, Vec<Edge>>,
}

impl CallGraph {
    /// The call graph of `module` as it stands (run it after DCE to see only
    /// live callers, before it to see every caller).
    #[must_use]
    pub fn new(module: &LirModule) -> Self {
        let mut callers: HashMap<Symbol, Vec<Edge>> = HashMap::new();
        for func in &module.funcs {
            for (target, span, kind) in callees(func) {
                callers.entry(target).or_default().push(Edge {
                    caller: func.name.clone(),
                    span,
                    kind,
                });
            }
        }
        Self { callers }
    }

    /// Every edge into `func`, in module order.
    #[must_use]
    pub fn callers(&self, func: &str) -> &[Edge] {
        self.callers.get(func).map_or(&[], Vec::as_slice)
    }

    /// The caller tree of `func`, whose own site is `site`: every call site
    /// that reaches it, with `.lilac.*` glue spliced out (its callers stand in
    /// its place). Each function's callers are expanded once; a later frame of
    /// it is a `repeat`. Names come from `names` (LIR symbol to source name).
    #[must_use]
    pub fn trace<S: BuildHasher>(
        &self,
        func: &str,
        site: Span,
        names: &HashMap<String, String, S>,
    ) -> TraceFrame {
        let mut expanded = HashSet::from([func.to_owned()]);
        TraceFrame {
            func: display(func, names),
            span: site,
            address: false,
            repeat: false,
            callers: self.frames(func, &mut expanded, names),
        }
    }

    /// The visible frames calling `func`.
    fn frames<S: BuildHasher>(
        &self,
        func: &str,
        expanded: &mut HashSet<String>,
        names: &HashMap<String, String, S>,
    ) -> Vec<TraceFrame> {
        let mut edges = Vec::new();
        let mut spliced = HashSet::from([func.to_owned()]);
        self.visible_edges(func, &mut spliced, &mut edges);
        edges
            .into_iter()
            .map(|edge| {
                let repeat = !expanded.insert(edge.caller.as_str().to_owned());
                let callers = if repeat {
                    Vec::new()
                } else {
                    self.frames(edge.caller.as_str(), expanded, names)
                };
                TraceFrame {
                    func: display(edge.caller.as_str(), names),
                    span: edge.span,
                    address: edge.kind == EdgeKind::Address,
                    repeat,
                    callers,
                }
            })
            .collect()
    }

    /// Edges into `func` from visible callers, looking through hidden ones.
    fn visible_edges<'a>(
        &'a self,
        func: &str,
        spliced: &mut HashSet<String>,
        out: &mut Vec<&'a Edge>,
    ) {
        for edge in self.callers(func) {
            if !is_hidden(edge.caller.as_str()) {
                out.push(edge);
            } else if spliced.insert(edge.caller.as_str().to_owned()) {
                self.visible_edges(edge.caller.as_str(), spliced, out);
            }
        }
    }
}

/// `func`'s source name, else the symbol itself.
fn display<S: BuildHasher>(func: &str, names: &HashMap<String, String, S>) -> String {
    names.get(func).cloned().unwrap_or_else(|| func.to_owned())
}

/// Walks the call graph breadth-first from `main` and `extra_roots`.
/// Returns `None` when there is no root (the pass is then a no-op).
#[must_use]
pub fn reach<S: BuildHasher>(
    module: &LirModule,
    extra_roots: &HashSet<String, S>,
) -> Option<Reach> {
    let funcs: HashMap<&str, &LirFunc> = module
        .funcs
        .iter()
        .map(|func| (func.name.as_str(), func))
        .collect();
    let mut reached: HashSet<Symbol> = HashSet::new();
    let mut queue: VecDeque<Symbol> = VecDeque::new();
    for func in &module.funcs {
        if func.name == "main" && reached.insert(func.name.clone()) {
            queue.push_back(func.name.clone());
        }
    }
    let mut extra: Vec<&String> = extra_roots.iter().collect();
    extra.sort();
    for root in extra {
        if reached.insert(root.into()) {
            queue.push_back(root.into());
        }
    }
    if queue.is_empty() {
        return None;
    }

    while let Some(name) = queue.pop_front() {
        let Some(func) = funcs.get(name.as_str()) else {
            continue;
        };
        for (target, _, _) in callees(func) {
            if funcs.contains_key(target.as_str()) && reached.insert(target.clone()) {
                queue.push_back(target);
            }
        }
    }
    Some(Reach { reached })
}

/// Removes functions unreachable from a root and data consts nothing names,
/// and returns the reachability it pruned by (`None` when it was a no-op).
///
/// Roots are every function named `main` plus every name in `extra_roots`
/// (build-system export hints). Reachability follows direct `call` and
/// `addr.func` targets (an address-taken function survives even when it is
/// never called directly). Data consts survive only when a surviving
/// instruction names them (a `const` of [`LirConst::Data`]). Layouts
/// survive when a surviving function's signature or instruction type names
/// them, directly or through another surviving layout's fields. With no `main`
/// and no `extra_roots` the pass is a no-op; a non-empty `extra_roots` prunes
/// even when `main` is absent, so an exported entry point roots a library.
pub fn prune_unreachable<S: BuildHasher>(
    module: &mut LirModule,
    extra_roots: &HashSet<String, S>,
) -> Option<Reach> {
    let reach = reach(module, extra_roots)?;

    module
        .funcs
        .retain(|func| reach.contains(func.name.as_str()));

    let mut referenced: HashSet<&Symbol> = HashSet::new();
    for func in &module.funcs {
        for block in &func.blocks {
            for inst in &block.insts {
                match inst {
                    LirInst::Const {
                        value: LirConst::Data(name),
                        ..
                    }
                    | LirInst::AddrGlobal { global: name, .. } => {
                        referenced.insert(name);
                    }
                    _ => {}
                }
            }
        }
    }
    let referenced: HashSet<Symbol> = referenced.into_iter().cloned().collect();
    module.data.retain(|datum| referenced.contains(&datum.name));
    module
        .globals
        .retain(|global| referenced.contains(&global.name));
    prune_layouts(module);
    Some(reach)
}

/// Keeps only the layouts the surviving functions use.
fn prune_layouts(module: &mut LirModule) {
    let mut pending: Vec<&LirTy> = module.globals.iter().map(|global| &global.ty).collect();
    for func in &module.funcs {
        pending.extend(func.ins.iter().map(|(_, ty)| ty));
        pending.extend(func.outs.iter().map(|(_, ty)| ty));
        for block in &func.blocks {
            pending.extend(block.insts.iter().filter_map(inst_ty));
        }
    }
    let mut used: HashSet<&str> = HashSet::new();
    while let Some(ty) = pending.pop() {
        match ty {
            LirTy::Ref(inner) | LirTy::Array(inner, _) => pending.push(inner),
            LirTy::Named(name) => {
                if used.insert(name) {
                    if let Some(fields) = module.layouts.get(name) {
                        pending.extend(fields.iter().map(|(_, ty)| ty));
                    }
                }
            }
            LirTy::Bit(_) | LirTy::BitNative | LirTy::FBit(_) => {}
        }
    }
    let used: HashSet<String> = used.into_iter().map(str::to_owned).collect();
    module.layouts.retain(|name, _| used.contains(name));
}

/// The type an instruction names, if any.
fn inst_ty(inst: &LirInst) -> Option<&LirTy> {
    match inst {
        LirInst::CopyReg { ty, .. }
        | LirInst::Const { ty, .. }
        | LirInst::Load { ty, .. }
        | LirInst::Addr { ty, .. }
        | LirInst::AddrFunc { ty, .. }
        | LirInst::AddrGlobal { ty, .. }
        | LirInst::Field { ty, .. }
        | LirInst::Convert { ty, .. }
        | LirInst::FieldAddr { ty, .. }
        | LirInst::ElemAddr { ty, .. }
        | LirInst::Alloca { ty, .. }
        | LirInst::Arith { ty, .. }
        | LirInst::AtomicLoad { ty, .. }
        | LirInst::AtomicStore { ty, .. }
        | LirInst::AtomicRmw { ty, .. }
        | LirInst::CmpXchg { ty, .. } => Some(ty),
        LirInst::CopyPlace { .. }
        | LirInst::Call { .. }
        | LirInst::StoreField { .. }
        | LirInst::Store { .. }
        | LirInst::Drop { .. }
        | LirInst::CompileError { .. }
        | LirInst::Zero { .. }
        | LirInst::Fence { .. } => None,
    }
}

/// Direct `call` and `addr.func` targets of every instruction in `func`, in
/// order, with their spans.
fn callees(func: &LirFunc) -> Vec<(Symbol, Span, EdgeKind)> {
    let mut targets = Vec::new();
    for block in &func.blocks {
        for inst in &block.insts {
            match inst {
                LirInst::Call {
                    func: LirCallee::Func(name),
                    span,
                    ..
                } => targets.push((name.clone(), *span, EdgeKind::Call)),
                LirInst::AddrFunc { func, span, .. } => {
                    targets.push((func.clone(), *span, EdgeKind::Address));
                }
                _ => {}
            }
        }
    }
    targets
}

#[cfg(test)]
mod tests {
    fn t(text: &str) -> crate::LirTy {
        crate::LirTy::parse(text).expect("type spelling")
    }

    use super::*;
    use crate::{LirBlock, LirData, LirTerm};
    use lilac_span::Span;

    /// Builds one function whose `entry` block calls each name in `calls`
    /// (directly) and takes the address of each name in `addr`.
    fn func(name: &str, calls: &[&str], addr: &[&str]) -> LirFunc {
        let mut insts = Vec::new();
        for callee in calls {
            insts.push(LirInst::Call {
                func: LirCallee::Func((*callee).into()),
                ins: Vec::new(),
                outs: Vec::new(),
                span: Span::dummy(),
            });
        }
        for (i, target) in addr.iter().enumerate() {
            insts.push(LirInst::AddrFunc {
                func: (*target).into(),
                dst: format!("a{i}").into(),
                ty: t("ref #bit8"),
                span: Span::dummy(),
            });
        }
        LirFunc {
            name: name.into(),
            ins: Vec::new(),
            outs: Vec::new(),
            is_extern: false,
            export: None,
            variadic: false,
            inline: false,
            blocks: vec![LirBlock {
                label: "entry".into(),
                insts,
                term: LirTerm::Return {
                    span: Span::dummy(),
                },
                span: Span::dummy(),
            }],
        }
    }

    /// Builds a module from function names plus data-const names. The root
    /// (`names[0]`) references `names[1]`: by direct `call` when more
    /// functions follow (leaving the tail dead), and by `addr.func` when
    /// exactly two functions exist (indirect-only reachability). The last
    /// function carries a `Const` reference to `data[0]` when data is given,
    /// so a dead function can pin otherwise-dead data.
    fn module_with(names: &[&str], data: &[&str]) -> LirModule {
        let mut module = LirModule::new("x86_64-unknown-none");
        for (i, name) in names.iter().enumerate() {
            let root = i == 0 && names.len() > 1;
            let calls: Vec<&str> = if root && names.len() > 2 {
                vec![names[1]]
            } else {
                Vec::new()
            };
            let addr: Vec<&str> = if root && names.len() == 2 {
                vec![names[1]]
            } else {
                Vec::new()
            };
            let mut f = func(name, &calls, &addr);
            if i + 1 == names.len() {
                if let Some(datum) = data.first() {
                    f.blocks[0].insts.push(LirInst::Const {
                        dst: "d".into(),
                        value: LirConst::Data((*datum).into()),
                        ty: t("#byte_buffer"),
                        span: Span::dummy(),
                    });
                }
            }
            module.funcs.push(f);
        }
        for name in data {
            module.data.push(LirData {
                name: (*name).into(),
                bytes: Vec::new(),
                span: Span::dummy(),
            });
        }
        module
    }

    #[test]
    fn sweeps_unreachable_funcs_and_data() {
        // main -> a; b is dead; !c0 is referenced only by b's Const.
        let mut module = module_with(&["main", "a", "b"], &["c0"]);
        prune_unreachable(&mut module, &HashSet::new());
        let names: Vec<&str> = module.funcs.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["main", "a"]);
        assert!(module.data.iter().all(|d| d.name != "c0"));
    }

    #[test]
    fn sweeps_layouts_no_survivor_names() {
        // main's alloca names #Outer, whose field names #Inner through a
        // `ref`; #Dead is named only by the dead function `b`.
        let mut module = module_with(&["main", "a", "b"], &[]);
        let alloca = |ty: &str| LirInst::Alloca {
            ty: t(ty),
            place: "x".into(),
            span: Span::dummy(),
        };
        module.funcs[0].blocks[0].insts.push(alloca("#Outer"));
        module.funcs[2].blocks[0].insts.push(alloca("#Dead"));
        module.layouts.insert(
            "Outer".to_owned(),
            vec![("inner".to_owned(), t("ref #Inner"))],
        );
        module
            .layouts
            .insert("Inner".to_owned(), vec![("value".to_owned(), t("#bit32"))]);
        module
            .layouts
            .insert("Dead".to_owned(), vec![("value".to_owned(), t("#bit32"))]);
        prune_unreachable(&mut module, &HashSet::new());
        let mut layouts: Vec<&str> = module.layouts.keys().map(String::as_str).collect();
        layouts.sort_unstable();
        assert_eq!(layouts, ["Inner", "Outer"]);
    }

    #[test]
    fn addr_func_target_survives() {
        // main takes `addr.func @only_indirect` but never `call`s it.
        let mut module = module_with(&["main", "only_indirect"], &[]);
        prune_unreachable(&mut module, &HashSet::new());
        let names: Vec<&str> = module.funcs.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["main", "only_indirect"]);
    }

    #[test]
    fn no_main_is_a_noop() {
        let mut module = module_with(&["a", "b"], &[]);
        let before = module.funcs.len();
        prune_unreachable(&mut module, &HashSet::new());
        assert_eq!(module.funcs.len(), before);
    }

    #[test]
    fn const_reference_retains_data() {
        // main -> a (via `addr.func`); only a names `!c0`.
        let mut module = module_with(&["main", "a"], &["c0"]);
        prune_unreachable(&mut module, &HashSet::new());
        assert!(
            module.data.iter().any(|d| d.name == "c0"),
            "a surviving Const must keep its data const"
        );
    }

    #[test]
    fn data_referenced_only_by_dead_func_is_swept() {
        // main -> a; b is dead and is the only referencer of `!c0`.
        let mut module = module_with(&["main", "a", "b"], &["c0"]);
        prune_unreachable(&mut module, &HashSet::new());
        assert!(
            module.data.is_empty(),
            "data referenced only by a swept function must be swept too"
        );
    }

    #[test]
    fn extra_root_survives_without_a_call() {
        // main -> a; b and api are never called, but api is exported.
        let mut module = module_with(&["main", "a", "b", "api"], &[]);
        let roots = HashSet::from(["api".to_owned()]);
        prune_unreachable(&mut module, &roots);
        let names: Vec<&str> = module.funcs.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["main", "a", "api"]);
    }

    #[test]
    fn extra_root_prunes_without_main() {
        // No `main`: `api` is exported and roots the module; `dead` is swept.
        let mut module = module_with(&["api"], &[]);
        module.funcs.push(func("dead", &[], &[]));
        let roots = HashSet::from(["api".to_owned()]);
        prune_unreachable(&mut module, &roots);
        let names: Vec<&str> = module.funcs.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["api"]);
    }

    /// `func [&] [^] (callers...)` for a trace, one level per parenthesis.
    fn shape(frame: &TraceFrame) -> String {
        let mut text = frame.func.clone();
        if frame.address {
            text.push_str(" &");
        }
        if frame.repeat {
            text.push_str(" ^");
        }
        if !frame.callers.is_empty() {
            let callers: Vec<String> = frame.callers.iter().map(shape).collect();
            text.push_str(" (");
            text.push_str(&callers.join(", "));
            text.push(')');
        }
        text
    }

    fn trace_of(module: &LirModule, func: &str) -> String {
        let names: HashMap<String, String> = HashMap::new();
        shape(&CallGraph::new(module).trace(func, Span::dummy(), &names))
    }

    #[test]
    fn trace_lists_every_call_site() {
        // main -> a -> c, main -> c, b -> c: c has three callers.
        let mut module = LirModule::new("x86_64-unknown-none");
        module.funcs.push(func("main", &["a", "c"], &[]));
        module.funcs.push(func("a", &["c"], &[]));
        module.funcs.push(func("b", &["c"], &[]));
        module.funcs.push(func("c", &[], &[]));
        assert_eq!(trace_of(&module, "c"), "c (main, a (main ^), b)");
    }

    #[test]
    fn trace_marks_recursion_as_repeat() {
        let mut module = LirModule::new("x86_64-unknown-none");
        module.funcs.push(func("main", &["f"], &[]));
        module.funcs.push(func("f", &["f"], &[]));
        assert_eq!(trace_of(&module, "f"), "f (main, f ^)");
    }

    #[test]
    fn trace_splices_out_hidden_glue() {
        // main -> .lilac.drop.T -> hook, and the glue recurses on itself.
        let mut module = LirModule::new("x86_64-unknown-none");
        module.funcs.push(func("main", &[".lilac.drop.T"], &[]));
        module
            .funcs
            .push(func(".lilac.drop.T", &["hook", ".lilac.drop.T"], &[]));
        module.funcs.push(func("hook", &[], &[]));
        assert_eq!(trace_of(&module, "hook"), "hook (main)");
    }

    #[test]
    fn trace_marks_an_address_edge() {
        let module = module_with(&["main", "only_indirect"], &[]);
        assert_eq!(trace_of(&module, "only_indirect"), "only_indirect (main &)");
    }

    #[test]
    fn trace_names_come_from_the_map() {
        let mut module = LirModule::new("x86_64-unknown-none");
        module.funcs.push(func(".root.f", &["g"], &[]));
        module.funcs.push(func("g", &[], &[]));
        let names = HashMap::from([(".root.f".to_owned(), "f".to_owned())]);
        let trace = CallGraph::new(&module).trace("g", Span::dummy(), &names);
        assert_eq!(trace.callers[0].func, "f");
    }

    #[test]
    fn no_root_reaches_nothing() {
        let module = module_with(&["a", "b"], &[]);
        assert!(reach(&module, &HashSet::<String>::new()).is_none());
    }
}
