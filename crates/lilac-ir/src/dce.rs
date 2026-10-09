//! Reachability sweep: drops functions with no path from `main`, data
//! consts no surviving instruction names, and layouts no surviving type
//! names.
//!
//! The frontend lowers every declaration, so a function that nothing calls and a
//! `!data` const nothing references still survive into `verify`. This pass is
//! conservative and infallible: with no `main` it leaves the module untouched.
//! The walk is breadth-first and remembers each function's first caller, so
//! [`Reach::chain`] gives a shortest call path from a root (the `E031` trace).

use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::BuildHasher;

use lilac_span::Span;

use crate::{LirCallee, LirFunc, LirInst, LirModule, LirTy};

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
    pub caller: String,
    /// Span of the `call` / `addr.func`.
    pub span: Span,
    /// Edge kind.
    pub kind: EdgeKind,
}

/// One frame of a [`Reach::chain`]: a function and the edge it takes toward
/// the previous (deeper) frame. The innermost frame has no edge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    /// Function name (LIR symbol).
    pub func: String,
    /// Span and kind of the edge to the previous frame.
    pub edge: Option<(Span, EdgeKind)>,
}

/// Functions reachable from the roots, each with its first-found parent edge
/// (`None` for a root). Breadth-first, so every chain is a shortest path.
#[derive(Clone, Debug, Default)]
pub struct Reach {
    parent: HashMap<String, Option<Edge>>,
}

impl Reach {
    /// Whether `func` is reachable.
    #[must_use]
    pub fn contains(&self, func: &str) -> bool {
        self.parent.contains_key(func)
    }

    /// The path from `func` up to its root, innermost first. Each frame past
    /// the first carries the edge it takes toward the frame before it. Empty
    /// when `func` is unreachable.
    #[must_use]
    pub fn chain(&self, func: &str) -> Vec<Frame> {
        let mut frames = Vec::new();
        let mut current = func;
        let mut edge_in: Option<(Span, EdgeKind)> = None;
        while let Some(parent) = self.parent.get(current) {
            frames.push(Frame {
                func: current.to_owned(),
                edge: edge_in,
            });
            match parent {
                Some(edge) => {
                    edge_in = Some((edge.span, edge.kind));
                    current = &edge.caller;
                }
                None => break,
            }
        }
        frames
    }
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
    let mut parent: HashMap<String, Option<Edge>> = HashMap::new();
    let mut queue: VecDeque<String> = VecDeque::new();
    for func in &module.funcs {
        if func.name == "main" && !parent.contains_key(&func.name) {
            parent.insert(func.name.clone(), None);
            queue.push_back(func.name.clone());
        }
    }
    let mut extra: Vec<&String> = extra_roots.iter().collect();
    extra.sort();
    for root in extra {
        if !parent.contains_key(root) {
            parent.insert(root.clone(), None);
            queue.push_back(root.clone());
        }
    }
    if queue.is_empty() {
        return None;
    }

    while let Some(name) = queue.pop_front() {
        let Some(func) = funcs.get(name.as_str()) else {
            continue;
        };
        for (target, span, kind) in callees(func) {
            if funcs.contains_key(target.as_str()) && !parent.contains_key(&target) {
                parent.insert(
                    target.clone(),
                    Some(Edge {
                        caller: name.clone(),
                        span,
                        kind,
                    }),
                );
                queue.push_back(target);
            }
        }
    }
    Some(Reach { parent })
}

/// Removes functions unreachable from a root and data consts nothing names,
/// and returns the reachability it pruned by (`None` when it was a no-op).
///
/// Roots are every function named `main` plus every name in `extra_roots`
/// (build-system export hints). Reachability follows direct `call` and
/// `addr.func` targets (an address-taken function survives even when it is
/// never called directly). Data consts survive only when a surviving
/// instruction names them: a `Const` literal starting with `'!'`. Layouts
/// survive when a surviving function's signature or instruction type names
/// them, directly or through another surviving layout's fields. With no `main`
/// and no `extra_roots` the pass is a no-op; a non-empty `extra_roots` prunes
/// even when `main` is absent, so an exported entry point roots a library.
pub fn prune_unreachable<S: BuildHasher>(
    module: &mut LirModule,
    extra_roots: &HashSet<String, S>,
) -> Option<Reach> {
    let reach = reach(module, extra_roots)?;

    module.funcs.retain(|func| reach.contains(&func.name));

    let mut referenced: HashSet<String> = HashSet::new();
    for func in &module.funcs {
        for block in &func.blocks {
            for inst in &block.insts {
                if let LirInst::Const { lit, .. } = inst {
                    if let Some(sym) = lit.strip_prefix('!') {
                        referenced.insert(sym.to_owned());
                    }
                }
            }
        }
    }
    module.data.retain(|datum| referenced.contains(&datum.name));
    prune_layouts(module);
    Some(reach)
}

/// Keeps only the layouts the surviving functions use.
fn prune_layouts(module: &mut LirModule) {
    let mut pending: Vec<&LirTy> = Vec::new();
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
            LirTy::Ref(inner) => pending.push(inner),
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
        | LirInst::Field { ty, .. }
        | LirInst::Convert { ty, .. }
        | LirInst::FieldAddr { ty, .. }
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
fn callees(func: &LirFunc) -> Vec<(String, Span, EdgeKind)> {
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
                func: LirCallee::Func((*callee).to_owned()),
                ins: Vec::new(),
                outs: Vec::new(),
                span: Span::dummy(),
            });
        }
        for (i, target) in addr.iter().enumerate() {
            insts.push(LirInst::AddrFunc {
                func: (*target).to_owned(),
                dst: format!("a{i}"),
                ty: t("ref #bit8"),
                span: Span::dummy(),
            });
        }
        LirFunc {
            name: name.to_owned(),
            ins: Vec::new(),
            outs: Vec::new(),
            is_extern: false,
            variadic: false,
            inline: false,
            blocks: vec![LirBlock {
                label: "entry".to_owned(),
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
                        dst: "d".to_owned(),
                        lit: format!("!{datum}"),
                        ty: t("#byte_buffer"),
                        span: Span::dummy(),
                    });
                }
            }
            module.funcs.push(f);
        }
        for name in data {
            module.data.push(LirData {
                name: (*name).to_owned(),
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
            place: "x".to_owned(),
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

    fn chain_names(reach: &Reach, func: &str) -> Vec<String> {
        reach
            .chain(func)
            .into_iter()
            .map(|frame| frame.func)
            .collect()
    }

    #[test]
    fn chain_is_the_shortest_path() {
        // main -> a -> b -> c and main -> c: the chain skips a and b.
        let mut module = LirModule::new("x86_64-unknown-none");
        module.funcs.push(func("main", &["a", "c"], &[]));
        module.funcs.push(func("a", &["b"], &[]));
        module.funcs.push(func("b", &["c"], &[]));
        module.funcs.push(func("c", &[], &[]));
        let reach = prune_unreachable(&mut module, &HashSet::new()).expect("rooted");
        assert_eq!(chain_names(&reach, "c"), ["c", "main"]);
        assert_eq!(chain_names(&reach, "b"), ["b", "a", "main"]);
        let frames = reach.chain("b");
        assert_eq!(frames[0].edge, None);
        assert_eq!(frames[1].edge.map(|(_, kind)| kind), Some(EdgeKind::Call));
    }

    #[test]
    fn chain_ends_at_an_extra_root() {
        let mut module = LirModule::new("x86_64-unknown-none");
        module.funcs.push(func("api", &["leaf"], &[]));
        module.funcs.push(func("leaf", &[], &[]));
        let roots = HashSet::from(["api".to_owned()]);
        let reach = prune_unreachable(&mut module, &roots).expect("rooted");
        assert_eq!(chain_names(&reach, "leaf"), ["leaf", "api"]);
        assert!(reach.chain("missing").is_empty());
    }

    #[test]
    fn chain_follows_an_address_edge() {
        let mut module = module_with(&["main", "only_indirect"], &[]);
        let reach = prune_unreachable(&mut module, &HashSet::new()).expect("rooted");
        let frames = reach.chain("only_indirect");
        assert_eq!(frames.len(), 2);
        assert_eq!(
            frames[1].edge.map(|(_, kind)| kind),
            Some(EdgeKind::Address)
        );
    }

    #[test]
    fn no_root_reaches_nothing() {
        let module = module_with(&["a", "b"], &[]);
        assert!(reach(&module, &HashSet::<String>::new()).is_none());
    }
}
