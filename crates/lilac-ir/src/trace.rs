//! Shadow-stack stack traces: the call-site text, and the pass that records
//! it.
//!
//! Each instrumented function keeps a `#.lilac.frame { prev, site }` record
//! in its own stack frame, linked onto the list `@trace_head` points at while
//! the function runs. Before each call the record's `site` is set to a
//! preformatted `"  at <function> (<file:line:col>)\n"` constant, so the
//! panic handler (`.lilac.panic`, see [`crate::panic_runtime`]) only copies
//! bytes when it walks the list.

use std::collections::HashMap;

use lilac_span::{SourceMap, Span};

use crate::{
    LirCallee, LirConst, LirData, LirFunc, LirInst, LirModule, LirOperand, LirTerm, LirTy, Reg,
    Symbol,
};

/// The frame record place and the place holding the head slot's address.
const FRAME: &str = ".lilac.frame";
const HEAD: &str = ".lilac.head";

/// One stack-trace line, newline included: `"  at main (main.lilac:3:5)\n"`,
/// or `"  at main\n"` for a compiler-generated call with no location. The
/// panic site (lowering) and every call site (the pass) are formatted here.
#[must_use]
pub fn site_text(func: &str, location: Option<&str>) -> String {
    match location {
        Some(location) => format!("  at {func} ({location})\n"),
        None => format!("  at {func}\n"),
    }
}

/// Whether `func` is compiler runtime or glue (`.lilac.*`): never
/// instrumented, and hidden from the `E031` reach chain.
#[must_use]
pub fn is_hidden(func: &str) -> bool {
    func.starts_with(".lilac.")
}

/// Instruments every user function with a shadow-stack frame.
///
/// - **Prologue** (entry block, after its leading `alloca`s, so every
///   `alloca` stays static): the frame and head places, `call @trace_head`,
///   `frame.prev ← *head`, `*head ← addr frame`.
/// - **Before each call** (not to a leaf builtin, which cannot panic):
///   `frame.site ← !site`, the call's [`site_text`] as a data constant,
///   deduplicated.
/// - **Before each `return`**: `*head ← frame.prev`.
///
/// `.lilac.*` runtime functions and `extern`s are skipped. `names` maps a
/// LIR symbol to its source name (`lilac_lower::Lowered::names`); a missing
/// one prints as the symbol. Run it after DCE.
pub fn instrument<S: std::hash::BuildHasher>(
    module: &mut LirModule,
    sources: &SourceMap,
    names: &HashMap<String, String, S>,
) {
    let mut sites = Sites::new(&module.data);
    for func in &mut module.funcs {
        if func.is_extern || is_hidden(func.name.as_str()) || func.blocks.is_empty() {
            continue;
        }
        let name = names
            .get(func.name.as_str())
            .cloned()
            .unwrap_or_else(|| func.name.as_str().to_owned());
        instrument_func(func, &name, sources, &mut sites);
    }
    module.data.extend(sites.added);
}

/// Site data constants: existing ones are reused by content, new ones are
/// named `.lilac.site.N`.
struct Sites {
    by_bytes: HashMap<Vec<u8>, Symbol>,
    added: Vec<LirData>,
}

impl Sites {
    fn new(data: &[LirData]) -> Self {
        let by_bytes = data
            .iter()
            .map(|datum| (datum.bytes.clone(), datum.name.clone()))
            .collect();
        Self {
            by_bytes,
            added: Vec::new(),
        }
    }

    fn intern(&mut self, text: String) -> Symbol {
        let bytes = text.into_bytes();
        if let Some(name) = self.by_bytes.get(&bytes) {
            return name.clone();
        }
        let name = Symbol::new(format!(".lilac.site.{}", self.added.len()));
        self.by_bytes.insert(bytes.clone(), name.clone());
        self.added.push(LirData {
            name: name.clone(),
            bytes,
            span: Span::dummy(),
        });
        name
    }
}

/// Allocates the frame and links it: `frame.prev ← *head`, `*head ← addr
/// frame`, through the registers `top` and `me`.
fn prologue(top: Reg, me: Reg, span: Span) -> [LirInst; 7] {
    let frame_ref = LirTy::trace_frame().to_ref();
    [
        LirInst::Alloca {
            ty: LirTy::trace_frame(),
            place: FRAME.into(),
            span,
        },
        LirInst::Alloca {
            ty: frame_ref.clone().to_ref(),
            place: HEAD.into(),
            span,
        },
        LirInst::Call {
            func: LirCallee::Builtin("@trace_head".to_owned()),
            ins: Vec::new(),
            outs: vec![HEAD.into()],
            span,
        },
        LirInst::Load {
            src: HEAD.into(),
            dst: top.clone(),
            ty: frame_ref.clone(),
            span,
        },
        LirInst::StoreField {
            value: LirOperand::Reg(top),
            obj: FRAME.into(),
            name: "prev".to_owned(),
            idx: 0,
            span,
        },
        LirInst::Addr {
            place: FRAME.into(),
            dst: me.clone(),
            ty: frame_ref,
            span,
        },
        LirInst::Store {
            src: LirOperand::Reg(me),
            dst: HEAD.into(),
            span,
        },
    ]
}

fn instrument_func(func: &mut LirFunc, name: &str, sources: &SourceMap, sites: &mut Sites) {
    let frame_ref = LirTy::trace_frame().to_ref();
    let mut regs = 0_u32;
    let mut reg = || {
        regs += 1;
        Reg::new(format!(".lilac.t{regs}"))
    };
    for (index, block) in func.blocks.iter_mut().enumerate() {
        let span = block.span;
        let mut old = std::mem::take(&mut block.insts).into_iter().peekable();
        let mut insts = Vec::new();
        if index == 0 {
            while let Some(alloca) = old.next_if(|inst| matches!(inst, LirInst::Alloca { .. })) {
                insts.push(alloca);
            }
            insts.extend(prologue(reg(), reg(), span));
        }
        for inst in old {
            if let LirInst::Call {
                func: callee,
                span: call_span,
                ..
            } = &inst
            {
                if !matches!(callee, LirCallee::Builtin(_)) {
                    let location = sources.location(*call_span);
                    let data = sites.intern(site_text(name, location.as_deref()));
                    let site = reg();
                    insts.push(LirInst::Const {
                        dst: site.clone(),
                        value: LirConst::Data(data),
                        ty: LirTy::byte_buffer(),
                        span: *call_span,
                    });
                    insts.push(LirInst::StoreField {
                        value: LirOperand::Reg(site),
                        obj: FRAME.into(),
                        name: "site".to_owned(),
                        idx: 1,
                        span: *call_span,
                    });
                }
            }
            insts.push(inst);
        }
        if matches!(block.term, LirTerm::Return { .. }) {
            let prev = reg();
            insts.extend([
                LirInst::Field {
                    obj: FRAME.into(),
                    name: "prev".to_owned(),
                    idx: 0,
                    dst: prev.clone(),
                    ty: frame_ref.clone(),
                    span,
                },
                LirInst::Store {
                    src: LirOperand::Reg(prev),
                    dst: HEAD.into(),
                    span,
                },
            ]);
        }
        block.insts = insts;
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use lilac_span::{SourceMap, Span};

    use super::instrument;
    use crate::{verify, LirBlock, LirCallee, LirFunc, LirInst, LirModule, LirOperand, LirTerm};
    use crate::{LirData, LirTy};

    fn call(func: &str, span: Span) -> LirInst {
        LirInst::Call {
            func: LirCallee::Func(func.into()),
            ins: Vec::new(),
            outs: Vec::new(),
            span,
        }
    }

    fn block(label: &str, insts: Vec<LirInst>, term: LirTerm) -> LirBlock {
        LirBlock {
            label: label.into(),
            insts,
            term,
            span: Span::dummy(),
        }
    }

    fn ret() -> LirTerm {
        LirTerm::Return {
            span: Span::dummy(),
        }
    }

    fn func(name: &str, blocks: Vec<LirBlock>) -> LirFunc {
        LirFunc {
            name: name.into(),
            ins: Vec::new(),
            outs: Vec::new(),
            is_extern: false,
            export: None,
            variadic: false,
            inline: false,
            blocks,
        }
    }

    /// `f` (a branch on its parameter to two returns, two calls at one span, a call in between),
    /// `g`, the runtime function `.lilac.x` and the `extern` `e`.
    fn module(sources: &mut SourceMap) -> LirModule {
        let file = sources.add_file("m.lilac", "f()\n  g()\n");
        let at = Span::new(6, 9, file);
        let mut module =
            LirModule::new(lilac_target::Target::parse("x86_64-unknown-none").expect("triple"));
        module.funcs.push(LirFunc {
            ins: vec![("p".into(), LirTy::Bit(8))],
            ..func(
                "f",
                vec![
                    block(
                        "entry",
                        vec![
                            LirInst::Alloca {
                                ty: LirTy::Bit(32),
                                place: "x".into(),
                                span: Span::dummy(),
                            },
                            call("g", at),
                            LirInst::CopyReg {
                                src: LirOperand::Place("p".into()),
                                dst: "c".into(),
                                ty: LirTy::Bit(8),
                                span: Span::dummy(),
                            },
                        ],
                        LirTerm::BranchIf {
                            cond: "c".into(),
                            then: "a".into(),
                            els: Some("b".into()),
                            span: Span::dummy(),
                        },
                    ),
                    block("a", vec![call("g", at)], ret()),
                    block("b", Vec::new(), ret()),
                ],
            )
        });
        module
            .funcs
            .push(func("g", vec![block("entry", Vec::new(), ret())]));
        module.funcs.push(func(
            ".lilac.x",
            vec![block("entry", vec![call("g", at)], ret())],
        ));
        module.funcs.push(LirFunc {
            is_extern: true,
            export: None,
            variadic: false,
            ..func("e", Vec::new())
        });
        module
    }

    fn calls_trace_head(inst: &LirInst) -> bool {
        matches!(inst, LirInst::Call { func: LirCallee::Builtin(name), .. } if name == "@trace_head")
    }

    /// Ends in `*head ← frame.prev`.
    fn unlinks(insts: &[LirInst]) -> bool {
        matches!(
            insts,
            [
                ..,
                LirInst::Field { obj, name, .. },
                LirInst::Store { dst, .. },
            ] if obj == ".lilac.frame" && name == "prev" && dst == ".lilac.head"
        )
    }

    #[test]
    fn instruments_user_functions() {
        let mut sources = SourceMap::new();
        let mut module = module(&mut sources);
        let names = HashMap::from([("f".to_owned(), "F".to_owned())]);
        instrument(&mut module, &sources, &names);

        let f = &module.funcs[0];
        let entry = &f.blocks[0].insts;
        // The allocas stay leading and static.
        assert!(matches!(&entry[0], LirInst::Alloca { place, .. } if place == "x"));
        assert!(matches!(&entry[1], LirInst::Alloca { place, .. } if place == ".lilac.frame"));
        assert!(matches!(&entry[2], LirInst::Alloca { place, .. } if place == ".lilac.head"));
        assert!(calls_trace_head(&entry[3]));

        // The call's site is stored just before it, under the source name.
        let call_at = entry
            .iter()
            .position(|inst| matches!(inst, LirInst::Call { func: LirCallee::Func(name), .. } if name == "g"))
            .expect("call kept");
        assert!(matches!(
            &entry[call_at - 1],
            LirInst::StoreField { obj, name, value: LirOperand::Reg(_), .. }
                if obj == ".lilac.frame" && name == "site"
        ));
        assert!(matches!(
            &entry[call_at - 2],
            LirInst::Const { value: crate::LirConst::Data(name), .. } if *name == ".lilac.site.0"
        ));
        let site: Vec<&LirData> = module
            .data
            .iter()
            .filter(|datum| datum.name.as_str().starts_with(".lilac.site."))
            .collect();
        // Both calls at the same span share one constant.
        assert_eq!(site.len(), 1);
        assert_eq!(site[0].bytes, b"  at F (m.lilac:2:3)\n");

        // Every return unlinks; a branch does not.
        assert!(!unlinks(entry));
        assert!(unlinks(&f.blocks[1].insts));
        assert!(unlinks(&f.blocks[2].insts));
        // `g` (no name given) is instrumented too.
        assert!(calls_trace_head(&module.funcs[1].blocks[0].insts[2]));

        // Runtime functions and externs are untouched.
        assert_eq!(module.funcs[2].blocks[0].insts.len(), 1);
        assert!(module.funcs[3].blocks.is_empty());

        let mut diags = lilac_error::Diags::new();
        assert!(
            verify(&module, &mut diags).is_ok(),
            "{:?}\n{}",
            diags.diagnostics(),
            module.to_text()
        );
    }
}
