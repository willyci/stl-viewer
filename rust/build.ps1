# Runs cargo with the PATH tweaks the windows-gnu toolchain needs.
#   .\build.ps1 build --release
#   .\build.ps1 run -- C:\models
#   .\build.ps1 test --workspace
#
# The Rust windows-gnu toolchain ships no assembler, so its bundled dlltool cannot
# build import libraries. LLVM's llvm-ar doubles as an assembler-free dlltool when
# invoked under that name, and gcc needs rust-lld's `ld.lld` shim on PATH for
# `-fuse-ld=lld` (see .cargo/config.toml). Not needed with the MSVC toolchain.
$ErrorActionPreference = 'Stop'
$root = $PSScriptRoot
$bin = Join-Path (rustc --print sysroot) 'lib\rustlib\x86_64-pc-windows-gnu\bin'
if (Test-Path (Join-Path $bin 'llvm-ar.exe')) {
    $tools = Join-Path $root '.tools'  # outside target/ so `cargo clean` keeps it
    New-Item -ItemType Directory -Force $tools | Out-Null
    $dlltool = Join-Path $tools 'dlltool.exe'
    if (-not (Test-Path $dlltool)) { Copy-Item (Join-Path $bin 'llvm-ar.exe') $dlltool }
    $env:PATH = "$tools;$(Join-Path $bin 'gcc-ld');$env:PATH"
} elseif ((rustc -vV | Select-String 'host:') -match 'windows-gnu') {
    Write-Warning 'llvm-ar not found -- run: rustup component add llvm-tools'
}
Push-Location $root
try { cargo @args; exit $LASTEXITCODE } finally { Pop-Location }
