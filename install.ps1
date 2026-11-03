[CmdletBinding()]
param(
    [ValidateSet('Install','Check','Download','Apply')][string]$Action = 'Install',
    [string]$SourceExe,
    [string]$PreparedDirectory,
    [int]$ParentId = 0,
    [long]$ParentStartTicks = 0,
    [switch]$Restart,
    [switch]$NoDesktopShortcut,
    [switch]$NoStartMenu,
    [switch]$FunctionsOnly
)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
[Console]::InputEncoding = [Text.UTF8Encoding]::new($false)
$script:Repository = 'ersinkoc/oh-my-backup'
$script:InstallRoot = [IO.Path]::GetFullPath((Join-Path $env:LOCALAPPDATA 'Programs/OhMyBackup'))
$script:UpdateRoot = [IO.Path]::GetFullPath((Join-Path $env:LOCALAPPDATA 'OhMyBackup/updates'))
$script:EngineMutex = 'Local\HBSync-single-instance'

function Assert-PlainPath([string]$Path) {
    $cursor = [IO.Path]::GetFullPath($Path)
    while ($cursor) {
        if (Test-Path -LiteralPath $cursor) {
            $item = Get-Item -LiteralPath $cursor -Force
            if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'Install/update paths cannot contain links or junctions.' }
        }
        $parent = [IO.Path]::GetDirectoryName($cursor)
        if ($parent -eq $cursor) { break }
        $cursor = $parent
    }
}

function Write-Json([string]$Path, $Value) {
    Assert-PlainPath $Path
    $temp = $Path + '.' + [guid]::NewGuid().ToString('N') + '.tmp'
    try {
        [IO.File]::WriteAllText($temp,($Value | ConvertTo-Json -Depth 8),[Text.UTF8Encoding]::new($false))
        if (Test-Path -LiteralPath $Path) { [IO.File]::Replace($temp,$Path,[NullString]::Value,$true) } else { [IO.File]::Move($temp,$Path) }
    } finally { if (Test-Path -LiteralPath $temp) { Remove-Item -LiteralPath $temp -Force } }
}

function Get-StableVersion([string]$Tag) {
    if ($Tag -notmatch '^v?(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$') { throw 'Invalid stable release version.' }
    return [version]$Tag.TrimStart('v')
}

function Get-Token([string]$Provided) {
    if ($Provided) { return (Assert-Token $Provided) }
    if ($env:GH_TOKEN) { return (Assert-Token $env:GH_TOKEN) }
    if ($env:GITHUB_TOKEN) { return (Assert-Token $env:GITHUB_TOKEN) }
    if (Get-Command gh -ErrorAction SilentlyContinue) {
        $token = & gh auth token --hostname github.com 2>$null
        if ($LASTEXITCODE -eq 0) { return (Assert-Token ([string]$token)) }
    }
    return ''
}

function Assert-Token([string]$Token) {
    if ($Token.Length -gt 4096 -or $Token -match '\s') { throw 'Invalid GitHub token: whitespace or excessive length.' }
    return $Token
}

function Invoke-Download([uri]$Uri, [string]$Token, [string]$Accept, [string]$OutFile, [long]$Limit) {
    Add-Type -AssemblyName System.Net.Http
    $handler = [Net.Http.HttpClientHandler]::new()
    $handler.AllowAutoRedirect = $false
    $client = [Net.Http.HttpClient]::new($handler)
    $client.Timeout = [TimeSpan]::FromSeconds(120)
    try {
        for ($redirect=0; $redirect -lt 6; $redirect++) {
            if ($Uri.Scheme -ne 'https' -or $Uri.UserInfo -or ($Uri.Host -ne 'api.github.com' -and $Uri.Host -ne 'github.com' -and $Uri.Host -notmatch '(^|\.)githubusercontent\.com$')) { throw 'Untrusted update URL.' }
            $request = [Net.Http.HttpRequestMessage]::new([Net.Http.HttpMethod]::Get,$Uri)
            $request.Headers.UserAgent.ParseAdd('OhMyBackup-Updater')
            $request.Headers.Accept.ParseAdd($Accept)
            if ($Uri.Host -eq 'api.github.com') {
                $request.Headers.Add('X-GitHub-Api-Version','2026-03-10')
                if ($Token) { $request.Headers.Authorization = [Net.Http.Headers.AuthenticationHeaderValue]::new('Bearer',$Token) }
            }
            $response = $client.SendAsync($request,[Net.Http.HttpCompletionOption]::ResponseHeadersRead).GetAwaiter().GetResult()
            try {
                if ([int]$response.StatusCode -in @(301,302,303,307,308)) {
                    $Uri = [uri]::new($Uri,$response.Headers.Location)
                    continue
                }
                if ([int]$response.StatusCode -in @(401,403,404)) { throw 'Release unavailable. For a private repository, configure a GitHub token with Contents: read or sign in with gh auth login.' }
                if (-not $response.IsSuccessStatusCode) { throw "GitHub request failed (HTTP $([int]$response.StatusCode))." }
                if ($response.Content.Headers.ContentLength -gt $Limit) { throw 'Update response exceeds size limit.' }
                $inputStream = $response.Content.ReadAsStreamAsync().GetAwaiter().GetResult()
                $output = [IO.File]::Open($OutFile,[IO.FileMode]::CreateNew,[IO.FileAccess]::Write,[IO.FileShare]::None)
                try {
                    $buffer = [byte[]]::new(65536); $total = 0L
                    while (($count = $inputStream.Read($buffer,0,$buffer.Length)) -gt 0) {
                        $total += $count
                        if ($total -gt $Limit) { throw 'Update response exceeds size limit.' }
                        $output.Write($buffer,0,$count)
                    }
                    $output.Flush($true)
                } finally { $output.Dispose(); $inputStream.Dispose() }
                return
            } finally { $response.Dispose(); $request.Dispose() }
        }
        throw 'Too many update redirects.'
    } finally { $client.Dispose(); $handler.Dispose() }
}

function New-UpdateDirectory {
    Assert-PlainPath $script:UpdateRoot
    [IO.Directory]::CreateDirectory($script:UpdateRoot) | Out-Null
    $path = Join-Path $script:UpdateRoot ([guid]::NewGuid().ToString('N'))
    [IO.Directory]::CreateDirectory($path) | Out-Null
    return $path
}

function Get-Release([string]$Token, [string]$Tag = '') {
    if ($Tag) { Get-StableVersion $Tag | Out-Null }
    $work = New-UpdateDirectory
    $endpoint = if ($Tag) { "tags/$Tag" } else { 'latest' }
    $json = Join-Path $work 'release.json'
    Invoke-Download "https://api.github.com/repos/$script:Repository/releases/$endpoint" $Token 'application/vnd.github+json' $json 2097152
    $release = Get-Content -LiteralPath $json -Raw | ConvertFrom-Json
    $version = Get-StableVersion $release.tag_name
    if ($release.draft -or $release.prerelease -or $release.tag_name -ne "v$version" -or ($Tag -and $release.tag_name -ne $Tag)) { throw 'A stable published release is required.' }
    $name = "oh-my-backup-v$version-windows-x64.zip"
    $zip = @($release.assets | Where-Object { $_.name -eq $name })
    $sum = @($release.assets | Where-Object { $_.name -eq 'SHA256SUMS.txt' })
    if ($zip.Count -ne 1 -or $sum.Count -ne 1 -or $zip[0].size -gt 104857600 -or $sum[0].size -gt 65536) { throw 'Release assets are missing, duplicated or oversized.' }
    return [pscustomobject]@{ version=$version.ToString(); tag=$release.tag_name; name=$name; asset_id=[long]$zip[0].id; checksum_id=[long]$sum[0].id; size=[long]$zip[0].size; digest=[string]$zip[0].digest }
}

function Assert-Checksum([string]$Archive, [string]$Sums, [string]$Name, [string]$Digest) {
    $matches = @(Get-Content -LiteralPath $Sums | Where-Object { $_ -match ('^[a-fA-F0-9]{64}\s+\*?' + [regex]::Escape($Name) + '$') })
    if ($matches.Count -ne 1) { throw 'Release checksum entry is missing or duplicated.' }
    $expected = ($matches[0] -split '\s+')[0].ToLowerInvariant()
    $actual = (Get-FileHash -LiteralPath $Archive -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actual -ne $expected -or ($Digest -and $Digest -ne "sha256:$actual")) { throw 'Release checksum verification failed.' }
}

function Expand-VerifiedPackage([string]$Archive, [string]$Destination) {
    Add-Type -AssemblyName System.IO.Compression
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $allowed = @('oh-my-backup.exe','install.ps1','README.md','CHANGELOG.md','LICENSE','hbsync.ico','logo.svg')
    $zip = [IO.Compression.ZipFile]::OpenRead($Archive)
    try {
        $seen = @{}; $total = 0L
        foreach ($entry in $zip.Entries) {
            $name = $entry.FullName.Replace('\','/')
            if ($name -eq 'OhMyBackup/') { continue }
            if ($name -notmatch '^OhMyBackup/([^/]+)$' -or $Matches[1] -notin $allowed) { throw 'Unexpected or unsafe archive path.' }
            $leaf = $Matches[1]
            if ($seen.ContainsKey($leaf) -or (($entry.ExternalAttributes -shr 16) -band 0xF000) -eq 0xA000) { throw 'Duplicate or symbolic-link archive entry.' }
            $seen[$leaf] = $true; $total += $entry.Length
            if ($entry.Length -gt 104857600 -or $total -gt 125829120) { throw 'Expanded package exceeds size limit.' }
        }
        if (-not $seen.ContainsKey('oh-my-backup.exe')) { throw 'Package executable missing.' }
        Assert-PlainPath $Destination
        [IO.Directory]::CreateDirectory($Destination) | Out-Null
        foreach ($entry in $zip.Entries) {
            if ($entry.FullName.Replace('\','/').EndsWith('/')) { continue }
            $leaf = [IO.Path]::GetFileName($entry.FullName.Replace('\','/'))
            [IO.Compression.ZipFileExtensions]::ExtractToFile($entry,(Join-Path $Destination $leaf),$false)
        }
    } finally { $zip.Dispose() }
}

function Get-ExecutableVersion([string]$Path) {
    # Bounded, console-free preflight, before touching the installed executable.
    $info = [Diagnostics.ProcessStartInfo]::new()
    $info.FileName = $Path; $info.Arguments = '--version'
    $info.UseShellExecute = $false; $info.CreateNoWindow = $true
    $info.RedirectStandardOutput = $true; $info.RedirectStandardError = $true
    $process = [Diagnostics.Process]::Start($info)
    try {
        if (-not $process.WaitForExit(15000)) { $process.Kill(); throw 'Executable version check timed out.' }
        $version = $process.StandardOutput.ReadToEnd().Trim()
        if ($process.ExitCode -ne 0 -or $version -notmatch '^Oh My Backup ([0-9]+\.[0-9]+\.[0-9]+)$') { throw 'Executable version check failed.' }
        return (Get-StableVersion $Matches[1]).ToString()
    } finally { $process.Dispose() }
}

function Get-PreparedPackage($Release, [string]$Token) {
    $work = New-UpdateDirectory
    $zip = Join-Path $work $Release.name; $sums = Join-Path $work 'SHA256SUMS.txt'
    Invoke-Download "https://api.github.com/repos/$script:Repository/releases/assets/$($Release.asset_id)" $Token 'application/octet-stream' $zip 104857600
    Invoke-Download "https://api.github.com/repos/$script:Repository/releases/assets/$($Release.checksum_id)" $Token 'application/octet-stream' $sums 65536
    if ((Get-Item -LiteralPath $zip).Length -ne $Release.size) { throw 'Release asset size mismatch.' }
    Assert-Checksum $zip $sums $Release.name $Release.digest
    $payload = Join-Path $work 'payload'
    Expand-VerifiedPackage $zip $payload
    $exe = Join-Path $payload 'oh-my-backup.exe'
    if ((Get-ExecutableVersion $exe) -ne $Release.version) { throw 'Release and executable versions differ.' }
    $descriptor = @{product='OhMyBackup'; version=$Release.version; sha256=(Get-FileHash -LiteralPath $exe -Algorithm SHA256).Hash; repository=$script:Repository}
    Write-Json (Join-Path $work 'prepared.json') $descriptor
    return [pscustomobject]@{version=$Release.version; directory=$work}
}

function Write-UpdateResult([string]$Status, [string]$Version) {
    Assert-PlainPath $script:UpdateRoot
    [IO.Directory]::CreateDirectory($script:UpdateRoot) | Out-Null
    Write-Json (Join-Path $script:UpdateRoot 'last-result.json') @{status=$Status; version=$Version; time=[DateTime]::UtcNow.ToString('o')}
}

function Resolve-InstallSource([string]$Exe, [string]$Version, [bool]$Explicit) {
    $target = Join-Path $script:InstallRoot 'oh-my-backup.exe'
    if (-not $Explicit -and (Test-Path -LiteralPath $target)) {
        $installed = Get-ExecutableVersion $target
        if ((Get-StableVersion $installed) -gt (Get-StableVersion $Version)) {
            return [pscustomobject]@{exe=$target; version=$installed}
        }
    }
    return [pscustomobject]@{exe=$Exe; version=$Version}
}

function Install-Executable([string]$Exe, [string]$Version, [bool]$Launch = $false, [bool]$RequireNewer = $false, [bool]$AllowDowngrade = $true) {
    Assert-PlainPath $script:InstallRoot
    [IO.Directory]::CreateDirectory($script:InstallRoot) | Out-Null
    foreach ($leaf in @('.install.lock','oh-my-backup.exe','oh-my-backup.previous.exe','install.json')) { Assert-PlainPath (Join-Path $script:InstallRoot $leaf) }
    $lock = [IO.File]::Open((Join-Path $script:InstallRoot '.install.lock'),[IO.FileMode]::OpenOrCreate,[IO.FileAccess]::ReadWrite,[IO.FileShare]::None)
    $target = Join-Path $script:InstallRoot 'oh-my-backup.exe'
    $candidate = Join-Path $script:InstallRoot ('candidate-' + [guid]::NewGuid().ToString('N') + '.exe')
    $previous = Join-Path $script:InstallRoot 'oh-my-backup.previous.exe'
    $replaced = $false; $manifestWritten = $false; $hadPrevious = Test-Path -LiteralPath $target
    $manifestPath = Join-Path $script:InstallRoot 'install.json'
    $oldManifest = $null
    try {
        $oldManifest = if (Test-Path -LiteralPath $manifestPath) { [IO.File]::ReadAllBytes($manifestPath) } else { $null }
        if ((Get-ExecutableVersion $Exe) -ne $Version) { throw 'Installer version preflight failed.' }
        if ($RequireNewer -and $hadPrevious -and (Get-StableVersion (Get-ExecutableVersion $target)) -ge (Get-StableVersion $Version)) { throw 'A newer executable is already installed.' }
        if (-not $AllowDowngrade -and $hadPrevious -and (Get-StableVersion (Get-ExecutableVersion $target)) -gt (Get-StableVersion $Version)) { throw 'A newer executable is already installed.' }
        $identical = $hadPrevious -and (Get-FileHash -LiteralPath $target).Hash -eq (Get-FileHash -LiteralPath $Exe).Hash
        if ($hadPrevious -and -not $identical) {
            $running = @(Get-Process -ErrorAction SilentlyContinue | Where-Object { try { $_.Path -eq $target } catch { $false } })
            if ($running.Count) { throw 'Close Oh My Backup before installing.' }
        }
        if (-not $identical) {
            Copy-Item -LiteralPath $Exe -Destination $candidate
            if ((Get-FileHash -LiteralPath $candidate).Hash -ne (Get-FileHash -LiteralPath $Exe).Hash) { throw 'Installer copy verification failed.' }
            if ($hadPrevious) { [IO.File]::Replace($candidate,$target,$previous,$true) } else { [IO.File]::Move($candidate,$target) }
            $replaced = $true
        }
        if ((Get-ExecutableVersion $target) -ne $Version) { throw 'Installed version check failed.' }
        foreach ($leaf in @('LICENSE','README.md','CHANGELOG.md','hbsync.ico','logo.svg')) {
            $source = Join-Path ([IO.Path]::GetDirectoryName($Exe)) $leaf
            if (-not (Test-Path -LiteralPath $source)) { $source = Join-Path $PSScriptRoot $leaf }
            $destination = Join-Path $script:InstallRoot $leaf
            Assert-PlainPath $destination
            if ((Test-Path -LiteralPath $source) -and [IO.Path]::GetFullPath($source) -ne $destination) { Copy-Item -LiteralPath $source -Destination $destination -Force }
        }
        Write-Json (Join-Path $script:InstallRoot 'install.json') @{product='OhMyBackup'; schema=1; version=$Version; repository=$script:Repository}
        $manifestWritten = $true
        # Update an existing startup entry without enabling startup for new users.
        Update-ExistingAutostart $target
        if ($Launch) { Start-Process -FilePath $target | Out-Null }
        Write-UpdateResult 'installed' $Version
    } catch {
        if ($replaced) {
            if ($hadPrevious -and (Test-Path -LiteralPath $previous)) { [IO.File]::Replace($previous,$target,[NullString]::Value,$true) }
            elseif (-not $hadPrevious) { Remove-Item -LiteralPath $target -Force }
            if ($manifestWritten) {
                if ($null -ne $oldManifest) { [IO.File]::WriteAllBytes($manifestPath,$oldManifest) }
                elseif (Test-Path -LiteralPath $manifestPath) { Remove-Item -LiteralPath $manifestPath -Force }
            }
            Write-UpdateResult 'rolled_back' $Version
        }
        throw
    } finally {
        if (Test-Path -LiteralPath $candidate) { Remove-Item -LiteralPath $candidate -Force }
        $lock.Dispose()
    }
}

function Update-ExistingAutostart([string]$Target) {
    $runKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
    if ((Get-ItemProperty -LiteralPath $runKey -Name HBSync -ErrorAction SilentlyContinue).HBSync) {
        Set-ItemProperty -LiteralPath $runKey -Name HBSync -Value ('"' + $Target + '"')
    }
}

function Invoke-ApplyUpdate([string]$Directory, [int]$OwnerId, [long]$OwnerStart, [bool]$Launch) {
    $directoryPath = [IO.Path]::GetFullPath($Directory)
    if ([IO.Path]::GetDirectoryName($directoryPath) -ne $script:UpdateRoot) { throw 'Update package is outside the update cache.' }
    Assert-PlainPath $directoryPath
    Assert-PlainPath (Join-Path $directoryPath 'payload/oh-my-backup.exe')
    $descriptor = Get-Content -LiteralPath (Join-Path $directoryPath 'prepared.json') -Raw | ConvertFrom-Json
    if ($descriptor.product -ne 'OhMyBackup' -or $descriptor.repository -ne $script:Repository) { throw 'Invalid prepared update.' }
    $version = (Get-StableVersion $descriptor.version).ToString()
    $exe = Join-Path $directoryPath 'payload/oh-my-backup.exe'
    if ((Get-FileHash -LiteralPath $exe -Algorithm SHA256).Hash -ne $descriptor.sha256) { throw 'Prepared executable changed after verification.' }
    $manifest = Get-Content -LiteralPath (Join-Path $script:InstallRoot 'install.json') -Raw | ConvertFrom-Json
    if ($manifest.product -ne 'OhMyBackup' -or (Get-StableVersion $manifest.version) -ge (Get-StableVersion $version)) { throw 'Update must be newer than the installed version.' }
    $actualVersion = Get-ExecutableVersion (Join-Path $script:InstallRoot 'oh-my-backup.exe')
    if ((Get-StableVersion $actualVersion) -ge (Get-StableVersion $version)) { throw 'Update must be newer than the installed executable.' }
    if ($OwnerId -gt 0) {
        $parent = Get-Process -Id $OwnerId -ErrorAction SilentlyContinue
        if ($parent -and $parent.StartTime.ToUniversalTime().ToFileTimeUtc() -eq $OwnerStart -and -not $parent.WaitForExit(120000)) { throw 'Application did not exit; installed files were not changed.' }
    }
    $mutex = [Threading.Mutex]::new($false,$script:EngineMutex); $owned = $false
    try {
        try { $owned = $mutex.WaitOne(0) } catch [Threading.AbandonedMutexException] { $owned = $true }
        if (-not $owned) { throw 'A backup engine is still running; update postponed.' }
        Install-Executable $exe $version $false $true $false
    } finally { if ($owned) { $mutex.ReleaseMutex() }; $mutex.Dispose() }
    if ($Launch) { Start-Process -FilePath (Join-Path $script:InstallRoot 'oh-my-backup.exe') | Out-Null }
}

function Set-AppShortcuts {
$targetExe = Join-Path $script:InstallRoot 'oh-my-backup.exe'
$Destination = $script:InstallRoot
    if (-not ('OhMyBackup.ShortcutIdentity' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Runtime.InteropServices.ComTypes;
namespace OhMyBackup {
 [ComImport, Guid("00021401-0000-0000-C000-000000000046")] class ShellLink {}
 [StructLayout(LayoutKind.Sequential, Pack=4)] struct PropertyKey {
  public Guid Format; public uint Id;
  public PropertyKey(Guid f, uint i) { Format=f; Id=i; }
 }
 [StructLayout(LayoutKind.Explicit, Size=24)] struct PropVariant {
  [FieldOffset(0)] public ushort Type;
  [FieldOffset(8)] public IntPtr Pointer;
 }
 [ComImport, Guid("886D8EEB-8CF2-4446-8D02-CDBA1DBDCF99"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
 interface IPropertyStore {
  uint GetCount(); void GetAt(uint index, out PropertyKey key);
  void GetValue(ref PropertyKey key, out PropVariant value);
  void SetValue(ref PropertyKey key, ref PropVariant value); void Commit();
 }
 public static class ShortcutIdentity {
  public static void Set(string path) {
   object link=new ShellLink();
   IntPtr value=Marshal.StringToCoTaskMemUni("HBSync.Desktop");
   try {
    var file=(IPersistFile)link; file.Load(path, 2);
    var key=new PropertyKey(new Guid("9F4C2855-9F79-4B39-A8D0-E1D42DE1D5F3"),5);
    var prop=new PropVariant { Type=31, Pointer=value };
    ((IPropertyStore)link).SetValue(ref key,ref prop);
    file.Save(path,true);
   } finally { Marshal.FreeCoTaskMem(value); Marshal.FinalReleaseComObject(link); }
  }
 }
}
'@
    }
$shell = New-Object -ComObject WScript.Shell
$directories = @(); if (-not $NoDesktopShortcut) { $directories += [Environment]::GetFolderPath('Desktop') }
if (-not $NoStartMenu) { $directories += Join-Path ([Environment]::GetFolderPath('Programs')) 'Oh My Backup' }
foreach ($directory in $directories) {
    [IO.Directory]::CreateDirectory($directory) | Out-Null
    $path = Join-Path $directory 'Oh My Backup.lnk'
    $link = $shell.CreateShortcut($path)
    $link.TargetPath = $targetExe
    $link.WorkingDirectory = $Destination
    $link.IconLocation = "$targetExe,0"
    $link.Description = 'Oh My Backup - Backup & Sync'
    $link.Save()
    [OhMyBackup.ShortcutIdentity]::Set($path)
    Write-Output "Shortcut created: $path"
}

}

if ($FunctionsOnly) { return }
try {
    switch ($Action) {
        'Check' {
            $request = [Console]::In.ReadToEnd() | ConvertFrom-Json
            $release = Get-Release (Get-Token $request.token)
            $release | ConvertTo-Json -Compress
        }
        'Download' {
            $request = [Console]::In.ReadToEnd() | ConvertFrom-Json
            $token = Get-Token $request.token
            $release = Get-Release $token $request.tag
            if ((Get-StableVersion $release.version) -le (Get-StableVersion $request.current_version)) { throw 'No newer update is available.' }
            Get-PreparedPackage $release $token | ConvertTo-Json -Compress
        }
        'Apply' { Invoke-ApplyUpdate $PreparedDirectory $ParentId $ParentStartTicks ([bool]$Restart) }
        'Install' {
            if (-not $SourceExe) {
                $bundled = Join-Path $PSScriptRoot 'oh-my-backup.exe'
                if (Test-Path -LiteralPath $bundled) { $SourceExe = $bundled }
            }
            if ($SourceExe) {
                $SourceExe = (Resolve-Path -LiteralPath $SourceExe).Path
                $version = Get-ExecutableVersion $SourceExe
            } else {
                $token = Get-Token ''
                $prepared = Get-PreparedPackage (Get-Release $token) $token
                $SourceExe = Join-Path $prepared.directory 'payload/oh-my-backup.exe'; $version = $prepared.version
            }
            $explicit = $PSBoundParameters.ContainsKey('SourceExe')
            $selected = Resolve-InstallSource $SourceExe $version $explicit
            $version = $selected.version
            Install-Executable $selected.exe $version ([bool]$Restart) $false $explicit
            $installedScript = Join-Path $script:InstallRoot 'install.ps1'
            if ([IO.Path]::GetFullPath($PSCommandPath) -ne $installedScript) { Copy-Item -LiteralPath $PSCommandPath -Destination $installedScript -Force }
            Set-AppShortcuts
            Write-Output "Installed Oh My Backup $version in $script:InstallRoot"
        }
    }
} catch {
    if ($Action -eq 'Apply') { Write-UpdateResult 'failed' '' }
    # Request headers and credential values never enter error output.
    [Console]::Error.WriteLine($_.Exception.Message)
    exit 1
}
