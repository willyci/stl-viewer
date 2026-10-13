# Source from Git Bash before running cargo:   source env.sh
# (The renamed tool lives in .tools/ so `cargo clean` does not delete it.)
#
# The Rust windows-gnu toolchain ships no assembler, so its bundled dlltool cannot
# build import libraries. LLVM's llvm-ar doubles as an assembler-free dlltool when
# invoked under that name, and gcc needs rust-lld's `ld.lld` shim on PATH for
# `-fuse-ld=lld` (see .cargo/config.toml).
_stlv_root="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
_stlv_bin="$(cygpath -u "$(rustc --print sysroot)")/lib/rustlib/x86_64-pc-windows-gnu/bin"
if [ ! -f "$_stlv_bin/llvm-ar.exe" ]; then
    echo "env.sh: llvm-ar not found -- run: rustup component add llvm-tools" >&2
else
    mkdir -p "$_stlv_root/.tools"
    [ -f "$_stlv_root/.tools/dlltool.exe" ] || cp "$_stlv_bin/llvm-ar.exe" "$_stlv_root/.tools/dlltool.exe"
    export PATH="$_stlv_root/.tools:$_stlv_bin/gcc-ld:$PATH"
fi
unset _stlv_root _stlv_bin
