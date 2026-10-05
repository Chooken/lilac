//! The Lilac bytecode virtual machine.
//!
//! This crate owns the M5c bytecode backend: the register-machine ISA and its
//! program model ([`isa`]), the verifier, the canonical `.lbc` text codec
//! ([`codec`]), and (in later tasks) the LIR compiler, interpreter, debugger,
//! and standalone runner. It never links LLVM.

pub mod codec;
pub mod compile;
pub mod exec;
pub mod isa;
pub use compile::compile;
pub use exec::{FrameInfo, Machine, Step, VmError};
pub use isa::*;
