param([switch]$SkipBuild)
$ErrorActionPreference = 'Stop'
$root = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
Push-Location $root
try {
    $version = (Select-String -LiteralPath Cargo.toml -Pattern '^version = "([0-9]+\.[0-9]+\.[0-9]+)"').Matches.Groups[1].Value
    if (-not $version) { throw 'Cargo.toml must contain a stable SemVer version.' }
    if (-not $SkipBuild) {
        $oldFlags = $env:CARGO_ENCODED_RUSTFLAGS
        try {
            $env:CARGO_ENCODED_RUSTFLAGS = "--remap-path-prefix=$root=/source$([char]31)--remap-path-prefix=$env:USERPROFILE=/toolchain"
            cargo build --release --locked
            if ($LASTEXITCODE -ne 0) { throw 'Release build failed.' }
        } finally { $env:CARGO_ENCODED_RUSTFLAGS = $oldFlags }
    }
    $out = Join-Path $root "dist/v$version"
    $app = Join-Path $out 'OhMyBackup'
    [IO.Directory]::CreateDirectory($app) | Out-Null
    Copy-Item -LiteralPath 'target/release/oh-my-backup.exe' -Destination $app -Force
    Copy-Item -LiteralPath 'scripts/install.ps1','README.md','CHANGELOG.md','assets/hbsync.ico','assets/logo.svg' -Destination $app -Force
    $zip = Join-Path $out "oh-my-backup-v$version-windows-x64.zip"
    Compress-Archive -LiteralPath $app -DestinationPath $zip -Force
    $sum = (Get-FileHash -LiteralPath $zip -Algorithm SHA256).Hash.ToLowerInvariant()
    [IO.File]::WriteAllText((Join-Path $out 'SHA256SUMS.txt'), "$sum  $([IO.Path]::GetFileName($zip))`n")
    Write-Output "Packaged v${version}: $zip"
} finally { Pop-Location }
