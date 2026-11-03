$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot '../install.ps1') -FunctionsOnly
$realVersionCheck = (Get-Command Get-ExecutableVersion).ScriptBlock
$testRoot = Join-Path ([IO.Path]::GetTempPath()) ('oh-my-backup-installer-' + [guid]::NewGuid().ToString('N'))
$script:InstallRoot = Join-Path $testRoot 'Programs/OhMyBackup'
$script:UpdateRoot = Join-Path $testRoot 'OhMyBackup/updates'
$script:EngineMutex = 'Local\OhMyBackupInstallerTest-' + [guid]::NewGuid().ToString('N')
[IO.Directory]::CreateDirectory($testRoot) | Out-Null
function Assert($Condition, [string]$Message) { if (-not $Condition) { throw $Message } }
function Expect-Error([scriptblock]$Run, [string]$Message) {
    $failed = $false
    try { & $Run | Out-Null } catch { $failed = $true }
    Assert $failed $Message
}
# Fixtures never execute and never touch Windows startup settings.
function Update-ExistingAutostart([string]$Target) { if ($script:FailAutostart) { throw 'Injected startup migration failure' } }
$script:FailAutostart = $false
$script:FailInstalledVersion = $false
function Get-ExecutableVersion([string]$Path) {
    if ($script:FailInstalledVersion -and $Path -eq (Join-Path $script:InstallRoot 'oh-my-backup.exe')) { throw 'Injected post-swap failure' }
    $text = [IO.File]::ReadAllText($Path)
    if ($text -notmatch '^fixture:([0-9]+\.[0-9]+\.[0-9]+)$') { throw 'Invalid test executable' }
    return $Matches[1]
}
function New-Fixture([string]$Version) {
    $path = Join-Path $testRoot "source-$Version.exe"
    [IO.File]::WriteAllText($path,"fixture:$Version")
    return $path
}
function New-Prepared([string]$Version) {
    $directory = New-UpdateDirectory
    [IO.Directory]::CreateDirectory((Join-Path $directory 'payload')) | Out-Null
    $exe = Join-Path $directory 'payload/oh-my-backup.exe'
    [IO.File]::WriteAllText($exe,"fixture:$Version")
    Write-Json (Join-Path $directory 'prepared.json') @{product='OhMyBackup'; repository=$script:Repository; version=$Version; sha256=(Get-FileHash -LiteralPath $exe).Hash}
    return $directory
}

Assert ((Get-StableVersion 'v0.10.0') -gt (Get-StableVersion '0.2.0')) 'Versions must compare numerically'
foreach ($value in @('1.2','1.2.3-beta','01.2.3','../1.2.3')) { Expect-Error { Get-StableVersion $value } 'Invalid version accepted' }

$old = New-Fixture '0.1.0'; $new = New-Fixture '0.2.0'
$script:FailInstalledVersion = $true
Expect-Error { Install-Executable $old '0.1.0' } 'First-install failure must propagate'
$script:FailInstalledVersion = $false
Assert (-not (Test-Path -LiteralPath (Join-Path $script:InstallRoot 'oh-my-backup.exe'))) 'Failed first installation left an executable'
Install-Executable $old '0.1.0'
Install-Executable $new '0.2.0'
$target = Join-Path $script:InstallRoot 'oh-my-backup.exe'
Assert (([IO.File]::ReadAllText($target)) -eq 'fixture:0.2.0') 'Upgrade did not replace executable'
Assert (([IO.File]::ReadAllText((Join-Path $script:InstallRoot 'oh-my-backup.previous.exe'))) -eq 'fixture:0.1.0') 'Previous executable not retained'
Install-Executable $new '0.2.0'
Assert (([IO.File]::ReadAllText((Join-Path $script:InstallRoot 'oh-my-backup.previous.exe'))) -eq 'fixture:0.1.0') 'Identical reinstall overwrote the useful previous executable'
Assert (([IO.File]::ReadAllBytes((Join-Path $script:InstallRoot 'install.json')))[0] -eq 123) 'Manifest must be UTF-8 without BOM'

$lock = [IO.File]::Open((Join-Path $script:InstallRoot '.install.lock'),[IO.FileMode]::Open,[IO.FileAccess]::ReadWrite,[IO.FileShare]::None)
try { Expect-Error { Install-Executable $new '0.2.0' } 'Competing installer was not blocked' } finally { $lock.Dispose() }

$third = New-Fixture '0.3.0'; $script:FailInstalledVersion = $true
Expect-Error { Install-Executable $third '0.3.0' } 'Injected post-swap failure must propagate'
$script:FailInstalledVersion = $false
Assert (([IO.File]::ReadAllText($target)) -eq 'fixture:0.2.0') 'Failed upgrade did not restore old executable'
Assert ((Get-Content (Join-Path $script:InstallRoot 'install.json') -Raw | ConvertFrom-Json).version -eq '0.2.0') 'Rollback changed installed manifest'

$prepared = New-Prepared '0.3.0'
$payload = Join-Path $prepared 'payload/oh-my-backup.exe'
[IO.File]::AppendAllText($payload,'tampered')
Expect-Error { Invoke-ApplyUpdate $prepared 0 0 $false } 'Tampered executable accepted'
Expect-Error { Invoke-ApplyUpdate $testRoot 0 0 $false } 'Outside-cache package accepted'
$prepared = New-Prepared '0.3.0'
$readyName = 'Local\OMB-ready-' + [guid]::NewGuid().ToString('N')
$releaseName = 'Local\OMB-release-' + [guid]::NewGuid().ToString('N')
$ready = [Threading.EventWaitHandle]::new($false,[Threading.EventResetMode]::ManualReset,$readyName)
$release = [Threading.EventWaitHandle]::new($false,[Threading.EventResetMode]::ManualReset,$releaseName)
$holderCode = @'
$mutex = [Threading.Mutex]::new($true,'@MUTEX@')
$ready = [Threading.EventWaitHandle]::OpenExisting('@READY@')
$release = [Threading.EventWaitHandle]::OpenExisting('@RELEASE@')
try { $ready.Set() | Out-Null; $release.WaitOne(30000) | Out-Null }
finally { $mutex.ReleaseMutex(); $mutex.Dispose(); $ready.Dispose(); $release.Dispose() }
'@
$holderCode = $holderCode.Replace('@MUTEX@',$script:EngineMutex).Replace('@READY@',$readyName).Replace('@RELEASE@',$releaseName)
$start = [Diagnostics.ProcessStartInfo]::new()
$start.FileName = Join-Path $env:SystemRoot 'System32/WindowsPowerShell/v1.0/powershell.exe'
$start.Arguments = '-NoProfile -NonInteractive -EncodedCommand ' + [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($holderCode))
$start.UseShellExecute = $false; $start.CreateNoWindow = $true
$holder = [Diagnostics.Process]::Start($start)
try {
    Assert ($ready.WaitOne(10000)) 'Mutex holder did not become ready'
    Expect-Error { Invoke-ApplyUpdate $prepared 0 0 $false } 'Update ignored an existing backup engine'
    Assert (([IO.File]::ReadAllText($target)) -eq 'fixture:0.2.0') 'Blocked update changed installed files'
} finally {
    $release.Set() | Out-Null
    if (-not $holder.WaitForExit(10000)) { $holder.Kill() }
    $holder.Dispose(); $ready.Dispose(); $release.Dispose()
}
Invoke-ApplyUpdate $prepared 0 0 $false
Assert (([IO.File]::ReadAllText($target)) -eq 'fixture:0.3.0') 'Prepared update did not install'
# Legacy installs have no manifest: a post-manifest failure must restore that absence.
Remove-Item -LiteralPath (Join-Path $script:InstallRoot 'install.json')
$fourth = New-Fixture '0.4.0'; $script:FailAutostart = $true
Expect-Error { Install-Executable $fourth '0.4.0' } 'Post-manifest failure must propagate'
$script:FailAutostart = $false
Assert (([IO.File]::ReadAllText($target)) -eq 'fixture:0.3.0') 'Legacy rollback changed executable'
Assert (-not (Test-Path -LiteralPath (Join-Path $script:InstallRoot 'install.json'))) 'Legacy rollback retained a new manifest'
Install-Executable $third '0.3.0'
$downgrade = New-Prepared '0.2.0'
Expect-Error { Invoke-ApplyUpdate $downgrade 0 0 $false } 'Downgrade accepted'
Write-Json (Join-Path $script:InstallRoot 'install.json') @{product='OhMyBackup'; schema=1; version='0.1.0'; repository=$script:Repository}
Expect-Error { Invoke-ApplyUpdate $downgrade 0 0 $false } 'Stale manifest allowed executable downgrade'
Assert (([IO.File]::ReadAllText($target)) -eq 'fixture:0.3.0') 'Stale-manifest check changed executable'
Expect-Error { Install-Executable $new '0.2.0' $false $true } 'Locked installer allowed an automatic downgrade'
$selection = Resolve-InstallSource $new '0.2.0' $false
Assert ($selection.version -eq '0.3.0' -and $selection.exe -eq $target) 'Bootstrap selected an older release over a newer install'
$selection = Resolve-InstallSource $new '0.2.0' $true
Assert ($selection.version -eq '0.2.0' -and $selection.exe -eq $new) 'Explicit local recovery should remain possible'
Expect-Error { Install-Executable $new '0.2.0' $false $false $false } 'Bootstrap downgrade guard failed under lock'

Add-Type -AssemblyName System.IO.Compression
Add-Type -AssemblyName System.IO.Compression.FileSystem
function New-Archive([string]$Name, [string[]]$Entries) {
    $path = Join-Path $testRoot $Name
    $zip = [IO.Compression.ZipFile]::Open($path,[IO.Compression.ZipArchiveMode]::Create)
    try {
        foreach ($entry in $Entries) {
            $stream = $zip.CreateEntry($entry).Open()
            try { $bytes = [Text.Encoding]::UTF8.GetBytes('fixture:0.3.0'); $stream.Write($bytes,0,$bytes.Length) } finally { $stream.Dispose() }
        }
    } finally { $zip.Dispose() }
    return $path
}
$archive = New-Archive 'valid.zip' @('OhMyBackup/oh-my-backup.exe','OhMyBackup/LICENSE')
Expand-VerifiedPackage $archive (Join-Path $testRoot 'expanded')
$sums = Join-Path $testRoot 'sums.txt'
$hash = (Get-FileHash -LiteralPath $archive).Hash.ToLowerInvariant()
[IO.File]::WriteAllText($sums,"$hash  valid.zip`n")
Assert-Checksum $archive $sums 'valid.zip' "sha256:$hash"
Expect-Error { Assert-Checksum $archive $sums 'valid.zip' ('sha256:' + ('0' * 64)) } 'Digest mismatch accepted'
[IO.File]::AppendAllText($archive,'tampered')
Expect-Error { Assert-Checksum $archive $sums 'valid.zip' '' } 'Bad checksum accepted'
foreach ($entries in @(@('OhMyBackup/../escape.exe'),@('OhMyBackup/oh-my-backup.exe','OhMyBackup/OH-MY-BACKUP.EXE'),@('other/oh-my-backup.exe'))) {
    $bad = New-Archive ([guid]::NewGuid().ToString('N') + '.zip') $entries
    Expect-Error { Expand-VerifiedPackage $bad (Join-Path $testRoot ([guid]::NewGuid().ToString('N'))) } 'Unsafe archive accepted'
}
Expect-Error { Invoke-Download 'http://example.invalid/file' '' 'application/json' (Join-Path $testRoot 'never') 100 } 'Non-HTTPS URL accepted'
$built = Join-Path $PSScriptRoot '../target/release/oh-my-backup.exe'
if (Test-Path -LiteralPath $built) { Assert ((& $realVersionCheck (Resolve-Path $built).Path) -match '^[0-9]+\.[0-9]+\.[0-9]+$') 'Real executable preflight failed' }
Write-Output 'PASS: installer upgrade, rollback, manifest encoding, lock ownership, tampering, downgrade, cache boundary, zip traversal, duplicate paths, checksums and HTTPS boundary.'
