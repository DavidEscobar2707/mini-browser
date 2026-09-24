@echo off
rem Build/run cargo for Mini. `cargo` is not on PATH on this machine and the
rem MSVC linker needs the VS environment, so both are set up here.
rem Usage: cmd.exe /c "tools\win-cargo.cmd" build
setlocal
set "VCVARS=C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat"
if not exist "%VCVARS%" set "VCVARS=C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Auxiliary\Build\vcvars64.bat"
if not exist "%VCVARS%" set "VCVARS=%ProgramFiles%\Microsoft Visual Studio\2022\Professional\VC\Auxiliary\Build\vcvars64.bat"
if not exist "%VCVARS%" (
  echo win-cargo: vcvars64.bat not found; install VS 2022 Build Tools. 1>&2
  exit /b 1
)
call "%VCVARS%" >nul
rem Build scripts (httparse and friends) shell out to a bare `rustc`.
set "PATH=%USERPROFILE%\.rustup\toolchains\stable-x86_64-pc-windows-msvc\bin;%PATH%"
cargo %*
