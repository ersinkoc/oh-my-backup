param(
    [string]$SourceExe = (Join-Path $PSScriptRoot 'oh-my-backup.exe'),
    [string]$Destination = (Join-Path $env:LOCALAPPDATA 'Programs/OhMyBackup'),
    [string]$ShortcutDirectory = [Environment]::GetFolderPath('Desktop'),
    [switch]$NoStartMenu
)
$ErrorActionPreference = 'Stop'
$SourceExe = (Resolve-Path -LiteralPath $SourceExe).Path
$Destination = [IO.Path]::GetFullPath($Destination)
[IO.Directory]::CreateDirectory($Destination) | Out-Null
$targetExe = Join-Path $Destination 'oh-my-backup.exe'
if ($SourceExe -ne $targetExe) { Copy-Item -LiteralPath $SourceExe -Destination $targetExe -Force }

# Give shortcuts the same explicit AppUserModelID as the running application.
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
$directories = @([IO.Path]::GetFullPath($ShortcutDirectory))
if (-not $NoStartMenu) { $directories += Join-Path ([Environment]::GetFolderPath('Programs')) 'Oh My Backup' }
foreach ($directory in $directories) {
    [IO.Directory]::CreateDirectory($directory) | Out-Null
    $path = Join-Path $directory 'Oh My Backup.lnk'
    $link = $shell.CreateShortcut($path)
    $link.TargetPath = $targetExe
    $link.WorkingDirectory = $Destination
    $link.IconLocation = "$targetExe,0"
    $link.Description = 'Oh My Backup — Backup & Sync'
    $link.Save()
    [OhMyBackup.ShortcutIdentity]::Set($path)
    Write-Output "Shortcut created: $path"
}
Write-Output "Installed: $targetExe"
Write-Output 'Pin the new shortcut to the taskbar. Existing pins may keep a cached icon until they are replaced.'
