@echo off
rem Runs cargo inside the Visual Studio x64 developer environment.
rem Only needed if `cargo build` fails with "linker `link.exe` not found" (Rust may not auto-detect newer VS
rem versions). Usage: tools\cargo-vs.cmd build --release
for /f "usebackq delims=" %%i in (`"%ProgramFiles(x86)%\Microsoft Visual Studio\Installer\vswhere.exe" -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath`) do set "VSDIR=%%i"
set "PATH=%ProgramFiles(x86)%\Microsoft Visual Studio\Installer;%USERPROFILE%\.cargo\bin;%PATH%"
call "%VSDIR%\VC\Auxiliary\Build\vcvars64.bat" >nul || exit /b 1
cargo %*
