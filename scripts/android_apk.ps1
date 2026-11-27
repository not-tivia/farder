# Builds a debug Android APK of Farder (64-bit ARM phones), on Windows, using the
# tools Android Studio installed. Result:
#   client\src-tauri\gen\android\app\build\outputs\apk\arm64\debug\app-arm64-debug.apk
#
#   .\scripts\android_apk.ps1
#
# Why not just `tauri android build`: it makes a symbolic link, which Windows only allows
# in Developer Mode (a system setting this script does not change). So the steps are done
# by hand: build the page, build the native library, copy it in, let Gradle package it.
# It does NOT install or run anything: that is a separate, deliberate step.
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$env:JAVA_HOME = "C:\Program Files\Android\Android Studio\jbr"
$env:ANDROID_HOME = Join-Path $env:LOCALAPPDATA "Android\Sdk"; $env:ANDROID_SDK_ROOT = $env:ANDROID_HOME
$ndk = Get-ChildItem (Join-Path $env:ANDROID_HOME "ndk") -Directory | Sort-Object Name -Descending | Select-Object -First 1
$env:NDK_HOME = $ndk.FullName
$env:PATH = "$env:JAVA_HOME\bin;" + $env:PATH

Write-Host "== 1/5 the page (embedded in the app) =="
Set-Location (Join-Path $root "client")
if (-not (Test-Path node_modules)) { npm ci }
npm run build; if ($LASTEXITCODE -ne 0) { throw "page build failed" }

Write-Host "== 2/5 the native library (compile + link for Android) =="
& (Join-Path $PSScriptRoot "portability_check_android.ps1") -Link
$so = Join-Path $root "target-android-check\aarch64-linux-android\debug\libfarder_client_lib.so"
if (-not (Test-Path $so)) { throw "native library was not produced" }

Write-Host "== 3/5 the Android project (once) =="
Set-Location (Join-Path $root "client")
if (-not (Test-Path "src-tauri\gen\android")) { npx tauri android init --ci }

Write-Host "== 4/5 put the library in (stripped of debug symbols to shrink it) =="
$dst = Join-Path $root "client\src-tauri\gen\android\app\src\main\jniLibs\arm64-v8a"
New-Item -ItemType Directory -Force -Path $dst | Out-Null
Copy-Item $so (Join-Path $dst "libfarder_client_lib.so") -Force
& (Join-Path $ndk.FullName "toolchains\llvm\prebuilt\windows-x86_64\bin\llvm-strip.exe") --strip-debug (Join-Path $dst "libfarder_client_lib.so")

Write-Host "== 5/5 package the APK (Gradle; its own Rust step is skipped, we did it above) =="
Set-Location (Join-Path $root "client\src-tauri\gen\android")
cmd /c "gradlew.bat assembleArm64Debug -x rustBuildArm64Debug --console=plain --no-daemon"
if ($LASTEXITCODE -ne 0) { throw "gradle failed" }
$apk = Join-Path $root "client\src-tauri\gen\android\app\build\outputs\apk\arm64\debug\app-arm64-debug.apk"
Write-Host ("APK: " + $apk + "  " + [math]::Round((Get-Item $apk).Length / 1MB, 1) + " MB")
