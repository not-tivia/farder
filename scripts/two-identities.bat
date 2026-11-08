@echo off
REM Double-click me: starts Farder as two identities (A and B).
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0two-identities.ps1"
