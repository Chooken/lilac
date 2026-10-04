//! Post-fold reachability sweep: drops functions with no path from `main`
//! and data consts no surviving instruction names.
//!
//! Folding removes dead *blocks* and dead `@compile_error` sites, but the
//! frontend lowers every declaration, so a function that nothing calls and a
//! `!data` const nothing references still survive into `verify`. This pass is
//! conservative and infallible: with no `main` it leaves the module untouched.

use std::collections::HashSet;

use crate::{LirCallee, LirFunc, LirInst, LirModule, LirTerm};

/// Removes functions unreachable from `main` and data consts nothing names.
///
/// Roots are every function named `main`. Reachability follows direct `call`
/// and `addr.func` targets (an address-taken function survives even when it is
/// never called directly). Data consts survive only when a surviving
/// instruction names them: a `Const` literal starting with `'!'`, or a `Panic`
/// message. `layouts` is verifier-only and is left untouched. With no `main`
/// the pass is a no-op.
pub fn prune_unreachable(module: &mut LirModule) {
    let mut visited: HashSet<String> = HashSet::new();
    let mut worklist: Vec<String> = Vec::new();
    for func in &module.funcs {
        if func.name == "main" && visited.insert(func.name.clone()) {
            worklist.push(func.name.clone());
        }
    }
    if worklist.is_empty() {
        return;
    }

    while let Some(name) = worklist.pop() {
        let targets = match module.funcs.iter().find(|func| func.name == name) {
            Some(func) => callees(func),
            None => continue,
        };
        for target in targets {
            let exists = module.funcs.iter().any(|func| func.name == target);
            if exists && visited.insert(target.clone()) {
                worklist.push(target);
            }
        }
    }

    module.funcs.retain(|func| visited.contains(&func.name));

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
            if let LirTerm::Panic { msg, .. } = &block.term {
                referenced.insert(msg.clone());
            }
        }
    }
    module.data.retain(|datum| referenced.contains(&datum.name));
}

/// Direct `call` and `addr.func` targets of every instruction in `func`.
fn callees(func: &LirFunc) -> Vec<String> {
    let mut targets = Vec::new();
    for block in &func.blocks {
        for inst in &block.insts {
            match inst {
                LirInst::Call {
                    func: LirCallee::Func(name),
                    ..
                } => targets.push(name.clone()),
                LirInst::AddrFunc { func, .. } => targets.push(func.clone()),
                _ => {}
            }
        }
    }
    targets
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LirBlock, LirData, LirDataKind};
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
                ty: "ref #bit8".to_owned(),
                span: Span::dummy(),
            });
        }
        LirFunc {
            name: name.to_owned(),
            ins: Vec::new(),
            outs: Vec::new(),
            is_extern: false,
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
                        ty: "#byte_buffer".to_owned(),
                        span: Span::dummy(),
                    });
                }
            }
            module.funcs.push(f);
        }
        for name in data {
            module.data.push(LirData {
                name: (*name).to_owned(),
                kind: LirDataKind::Str(String::new()),
                span: Span::dummy(),
            });
        }
        module
    }

    #[test]
    fn sweeps_unreachable_funcs_and_data() {
        // main -> a; b is dead; !c0 is referenced only by b's Const.
        let mut module = module_with(&["main", "a", "b"], &["c0"]);
        prune_unreachable(&mut module);
        let names: Vec<&str> = module.funcs.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["main", "a"]);
        assert!(module.data.iter().all(|d| d.name != "c0"));
    }

    #[test]
    fn addr_func_target_survives() {
        // main takes `addr.func @only_indirect` but never `call`s it.
        let mut module = module_with(&["main", "only_indirect"], &[]);
        prune_unreachable(&mut module);
        let names: Vec<&str> = module.funcs.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["main", "only_indirect"]);
    }

    #[test]
    fn no_main_is_a_noop() {
        let mut module = module_with(&["a", "b"], &[]);
        let before = module.funcs.len();
        prune_unreachable(&mut module);
        assert_eq!(module.funcs.len(), before);
    }

    #[test]
    fn const_reference_retains_data() {
        // main -> a (via `addr.func`); only a names `!c0`.
        let mut module = module_with(&["main", "a"], &["c0"]);
        prune_unreachable(&mut module);
        assert!(
            module.data.iter().any(|d| d.name == "c0"),
            "a surviving Const must keep its data const"
        );
    }

    #[test]
    fn panic_message_retains_data() {
        // main's terminator is the only referencer of `!c0`.
        let mut module = module_with(&["main"], &["c0"]);
        module.funcs[0].blocks[0].insts.clear();
        module.funcs[0].blocks[0].term = LirTerm::Panic {
            msg: "c0".to_owned(),
            span: Span::dummy(),
        };
        prune_unreachable(&mut module);
        assert!(
            module.data.iter().any(|d| d.name == "c0"),
            "a surviving Panic must keep its data const"
        );
    }

    #[test]
    fn data_referenced_only_by_dead_func_is_swept() {
        // main -> a; b is dead and is the only referencer of `!c0`.
        let mut module = module_with(&["main", "a", "b"], &["c0"]);
        prune_unreachable(&mut module);
        assert!(
            module.data.is_empty(),
            "data referenced only by a swept function must be swept too"
        );
    }
}
