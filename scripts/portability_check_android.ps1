# Type-checks Farder's shared Rust crates (and, with -Client, the Tauri app crate)
# for Android using the NDK that Android Studio installed. Run on Windows:
#   .\scripts\portability_check_android.ps1 [-Client]
# Uses its own build folder so it never disturbs a normal build.
param([switch]$Client, [switch]$Link)
$ErrorActionPreference = "Continue"
$sdk = Join-Path $env:LOCALAPPDATA "Android\Sdk\ndk"
$ndk = Get-ChildItem $sdk -Directory | Sort-Object Name -Descending | Select-Object -First 1
if (-not $ndk) { throw "no Android NDK under $sdk" }
$bin = Join-Path $ndk.FullName "toolchains\llvm\prebuilt\windows-x86_64\bin"
$triple = "aarch64-linux-android"
$env:CC_aarch64_linux_android = Join-Path $bin "aarch64-linux-android24-clang.cmd"
$env:CXX_aarch64_linux_android = Join-Path $bin "aarch64-linux-android24-clang++.cmd"
$env:AR_aarch64_linux_android = Join-Path $bin "llvm-ar.exe"
$env:CMAKE_POLICY_VERSION_MINIMUM = "3.5"
# The cmake crate finds the Android toolchain through these.
$env:ANDROID_NDK_HOME = $ndk.FullName
$env:ANDROID_NDK = $ndk.FullName
$env:ANDROID_NDK_ROOT = $ndk.FullName
# C libraries built with CMake (Opus): use the NDK's own toolchain file and Ninja,
# not the Visual Studio generator Windows would pick by default.
$sdkCmake = Get-ChildItem (Join-Path $env:LOCALAPPDATA "Android\Sdk\cmake") -Directory | Sort-Object Name -Descending | Select-Object -First 1
if ($sdkCmake) { $env:PATH = (Join-Path $sdkCmake.FullName "bin") + ";" + $env:PATH }
$env:CMAKE_GENERATOR = "Ninja"
$env:CMAKE_TOOLCHAIN_FILE_aarch64_linux_android = Join-Path $PSScriptRoot "android-toolchain.cmake"
$env:CARGO_TARGET_DIR = Join-Path (Split-Path -Parent $PSScriptRoot) "target-android-check"
rustup target add $triple | Out-Null
# A leftover CMake cache from a run with a different generator makes CMake refuse to continue; remove the whole
# build-script folder (not just its output) so Cargo really re-runs it and finds the library it builds.
Get-ChildItem (Join-Path $env:CARGO_TARGET_DIR "$triple\debug\build") -Directory -Filter "audiopus_sys-*" -ErrorAction SilentlyContinue | ForEach-Object { Remove-Item -Recurse -Force $_.FullName -ErrorAction SilentlyContinue }
Set-Location (Split-Path -Parent $PSScriptRoot)
Write-Host "NDK: $($ndk.Name)"
$crates = @("farder-protocol", "farder-crypto", "farder-history", "farder-e2ee-client", "farder-mls")
function Check($name, $dir) {
    Push-Location $dir
    $out = cargo check --target $triple 2>&1 | Out-String
    Pop-Location
    $errs = ($out -split "`n" | Where-Object { $_ -match "^error" }) -join "`n"
    if ($errs) { Write-Host "FAIL  $name"; ($errs -split "`n" | Select-Object -First 8) | ForEach-Object { Write-Host "      $_" } }
    else { Write-Host "ok    $name" }
}
if ($Link) {
    # Really compile and LINK the app library for Android (produces libfarder_client_lib.so).
    $env:CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER = Join-Path $bin "aarch64-linux-android24-clang.cmd"
    Push-Location (Join-Path (Get-Location) "client\src-tauri")
    cargo build --lib --target $triple 2>&1 | Select-Object -Last 25
    Pop-Location
    $so = Join-Path $env:CARGO_TARGET_DIR "$triple\debug\libfarder_client_lib.so"
    if (Test-Path $so) { Write-Host ("LINKED: " + $so + "  " + [math]::Round((Get-Item $so).Length / 1MB, 1) + " MB") } else { Write-Host "NOT LINKED" }
    exit
}
if ($Client) { Check "farder-client (the Tauri app)" (Join-Path (Get-Location) "client\src-tauri"); exit }
foreach ($c in $crates) {
    $out = cargo check -p $c --target $triple 2>&1 | Out-String
    $errs = ($out -split "`n" | Where-Object { $_ -match "^error" }) -join "`n"
    if ($errs) { Write-Host "FAIL  $c"; ($errs -split "`n" | Select-Object -First 4) | ForEach-Object { Write-Host "      $_" } }
    else { Write-Host "ok    $c" }
}
