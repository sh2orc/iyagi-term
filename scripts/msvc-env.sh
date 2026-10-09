# MSVC build environment for Git Bash shells (repo-local helper).
# Toolchain pieces: VS 2026 link.exe (system partial install) + xwin-splatted
# MSVC CRT headers/libs + Windows SDK 26100 (winget), because the machine's VS
# installs lack the C++ workload headers. Source before cargo.
XWIN="D:\project\iyagi\target\xwin"
export PATH="/c/Program Files/Microsoft Visual Studio/18/Community/VC/Tools/MSVC/14.51.36231/bin/HostX64/x64:$PATH"
export INCLUDE="$XWIN\crt\include;$XWIN\sdk\include\um;$XWIN\sdk\include\shared;$XWIN\sdk\include\ucrt"
export LIB="$XWIN\crt\lib\x86_64;$XWIN\sdk\lib\um\x86_64;$XWIN\sdk\lib\ucrt\x86_64"
