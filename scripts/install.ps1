# Compatibility entry point. The self-contained installer lives at the repository/package root.
& (Join-Path $PSScriptRoot '../install.ps1') @args
if (-not $?) { exit 1 }
