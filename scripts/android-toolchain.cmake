# CMake toolchain used by scripts/portability_check_android.ps1: the NDK's own file,
# told to build for 64-bit ARM phones (it defaults to 32-bit otherwise).
set(ANDROID_ABI arm64-v8a)
set(ANDROID_PLATFORM android-24)
include("$ENV{ANDROID_NDK_HOME}/build/cmake/android.toolchain.cmake")
