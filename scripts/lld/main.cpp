// `lld` with every driver, for a self-contained sysroot: built by
// `scripts/build-lld.sh` from LLVM's static libraries where the LLVM tree's
// own `lld` loads `libLLVM.dylib` (Homebrew's on macOS).
#include "lld/Common/Driver.h"
#include "llvm/Support/raw_ostream.h"

#include <vector>

LLD_HAS_DRIVER(coff)
LLD_HAS_DRIVER(elf)
LLD_HAS_DRIVER(mingw)
LLD_HAS_DRIVER(macho)
LLD_HAS_DRIVER(wasm)

int main(int argc, const char **argv) {
  std::vector<const char *> args(argv, argv + argc);
  return lld::lldMain(args, llvm::outs(), llvm::errs(), LLD_ALL_DRIVERS).retCode;
}
