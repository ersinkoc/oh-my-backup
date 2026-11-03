param([Parameter(Mandatory=$true)][string]$PreviousExecutable, [Parameter(Mandatory=$true)][string]$NewExecutable)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot '../install.ps1') -FunctionsOnly
function Update-ExistingAutostart([string]$Target) {}
$root = Join-Path ([IO.Path]::GetTempPath()) ('oh-my-backup-real-upgrade-' + [guid]::NewGuid().ToString('N'))
$script:InstallRoot = Join-Path $root 'Programs/OhMyBackup'
$script:UpdateRoot = Join-Path $root 'OhMyBackup/updates'
$script:EngineMutex = 'Local\OhMyBackupRealUpgradeTest-' + [guid]::NewGuid().ToString('N')
$PreviousExecutable = (Resolve-Path -LiteralPath $PreviousExecutable).Path
$NewExecutable = (Resolve-Path -LiteralPath $NewExecutable).Path
$oldVersion = Get-ExecutableVersion $PreviousExecutable
$newVersion = Get-ExecutableVersion $NewExecutable
if ((Get-StableVersion $newVersion) -le (Get-StableVersion $oldVersion)) { throw 'Test needs a genuinely newer executable' }
Install-Executable $PreviousExecutable $oldVersion
$directory = New-UpdateDirectory
[IO.Directory]::CreateDirectory((Join-Path $directory 'payload')) | Out-Null
$candidate = Join-Path $directory 'payload/oh-my-backup.exe'
Copy-Item -LiteralPath $NewExecutable -Destination $candidate
Write-Json (Join-Path $directory 'prepared.json') @{product='OhMyBackup'; repository=$script:Repository; version=$newVersion; sha256=(Get-FileHash -LiteralPath $candidate).Hash}
Invoke-ApplyUpdate $directory 0 0 $false
$target = Join-Path $script:InstallRoot 'oh-my-backup.exe'
if ((Get-ExecutableVersion $target) -ne $newVersion) { throw 'Installed executable version mismatch' }
if ((Get-FileHash -LiteralPath $target).Hash -ne (Get-FileHash -LiteralPath $NewExecutable).Hash) { throw 'Installed executable content mismatch' }
if ((Get-FileHash -LiteralPath (Join-Path $script:InstallRoot 'oh-my-backup.previous.exe')).Hash -ne (Get-FileHash -LiteralPath $PreviousExecutable).Hash) { throw 'Previous executable not retained' }
Write-Output "PASS: real executable upgraded $oldVersion -> $newVersion in isolated fixed-layout installation; hashes, manifest and previous executable verified."
