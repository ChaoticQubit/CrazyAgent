$ErrorActionPreference = 'Stop'
Set-StrictMode -Version 2

$principal = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'the share-stop setup needs the elevated runner'
}
$ip = (Get-NetIPAddress -AddressFamily IPv4 | Where-Object { $_.IPAddress -notlike '127.*' -and $_.IPAddress -notlike '169.254.*' } | Select-Object -First 1).IPAddress
if (-not $ip) { throw 'the runner has no IPv4 address outside loopback and link-local' }
if ((Get-Service LanmanServer).Status -ne 'Running') { throw 'the SMB server is not running' }

$shares = Join-Path $env:RUNNER_TEMP 'unc-shares'
$names = @('unc-alternates', 'unc-nested', 'unc-include', 'unc-commondir', 'unc-exclude')
foreach ($name in $names) {
    $folder = Join-Path $shares $name
    New-Item -ItemType Directory -Force -Path $folder | Out-Null
    $made = (& net share "$name=$folder" '/GRANT:Everyone,READ' 2>&1 | Out-String).Trim()
    if ($LASTEXITCODE -ne 0) { throw "net share $name failed: $made" }
}
$work = Join-Path $env:RUNNER_TEMP 'unc-nested-work'
& git init -q -b main $work
if ($LASTEXITCODE -ne 0) { throw 'git init of the nested share work tree failed' }
Set-Content -NoNewline -Path (Join-Path $work 'n.txt') -Value 'nested'
& git -C $work add n.txt
if ($LASTEXITCODE -ne 0) { throw 'git add in the nested share work tree failed' }
& git -C $work -c user.name=share -c user.email=share@example.invalid -c commit.gpgSign=false commit -q -m nested
if ($LASTEXITCODE -ne 0) { throw 'git commit in the nested share work tree failed' }
$nested = Join-Path $shares 'unc-nested\nested'
& git clone -q --bare $work $nested
if ($LASTEXITCODE -ne 0) { throw 'git clone --bare into the nested share failed' }
$nestedHead = (& git -c safe.directory=* --git-dir=$nested rev-parse HEAD | Out-String).Trim()
if ($LASTEXITCODE -ne 0 -or $nestedHead -notmatch '^[0-9a-f]{40}$') { throw "the nested share repository has no commit: $nestedHead" }
Add-Content -Path $env:GITHUB_ENV -Value "CRAZYAGENT_FARM_NESTED_HEAD=$nestedHead"
Set-Content -NoNewline -Path (Join-Path $shares 'unc-exclude\exclude') -Value 'linked-exclude'
Set-Content -NoNewline -Path (Join-Path $shares 'unc-include\cfg') -Value "[user]`n`tname = share`n"

Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Security.AccessControl;
using System.Security.Principal;
using Microsoft.Win32.SafeHandles;
public static class DeviceAccess {
    [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    static extern SafeFileHandle CreateFileW(string name, uint access, uint share, IntPtr attributes, uint disposition, uint flags, IntPtr template);
    [DllImport("advapi32.dll", SetLastError = true)]
    static extern bool GetKernelObjectSecurity(SafeFileHandle handle, uint information, byte[] descriptor, uint length, out uint needed);
    [DllImport("advapi32.dll", SetLastError = true)]
    static extern bool SetKernelObjectSecurity(SafeFileHandle handle, uint information, byte[] descriptor);
    const uint ReadControl = 0x20000;
    const uint WriteDac = 0x40000;
    const uint Dacl = 4;
    static SafeFileHandle Open(string path, bool folder) {
        SafeFileHandle handle = CreateFileW(path, ReadControl | WriteDac, 7, IntPtr.Zero, 3, folder ? 0x02000000u : 0u, IntPtr.Zero);
        if (handle.IsInvalid) { throw new Win32Exception(Marshal.GetLastWin32Error()); }
        return handle;
    }
    static RawSecurityDescriptor Read(SafeFileHandle handle) {
        uint needed;
        GetKernelObjectSecurity(handle, Dacl, null, 0, out needed);
        byte[] bytes = new byte[needed];
        if (!GetKernelObjectSecurity(handle, Dacl, bytes, needed, out needed)) { throw new Win32Exception(Marshal.GetLastWin32Error()); }
        return new RawSecurityDescriptor(bytes, 0);
    }
    public static string Allow(string path, bool folder, string sid, int mask) {
        using (SafeFileHandle handle = Open(path, folder)) {
            RawSecurityDescriptor descriptor = Read(handle);
            SecurityIdentifier who = new SecurityIdentifier(sid);
            RawAcl list = descriptor.DiscretionaryAcl;
            for (int at = list.Count - 1; at >= 0; at--) {
                CommonAce entry = list[at] as CommonAce;
                if (entry != null && entry.SecurityIdentifier == who && entry.AceQualifier == AceQualifier.AccessAllowed && entry.AceFlags == AceFlags.None) { list.RemoveAce(at); }
            }
            list.InsertAce(list.Count, new CommonAce(AceFlags.None, AceQualifier.AccessAllowed, mask, who, false, null));
            descriptor.DiscretionaryAcl = list;
            byte[] bytes = new byte[descriptor.BinaryLength];
            descriptor.GetBinaryForm(bytes, 0);
            if (!SetKernelObjectSecurity(handle, Dacl, bytes)) { throw new Win32Exception(Marshal.GetLastWin32Error()); }
            return Read(handle).GetSddlForm(AccessControlSections.Access);
        }
    }
}
'@

$packages = 'S-1-15-2-1'
Write-Host "GRANT nul $([DeviceAccess]::Allow('\\.\NUL', $false, $packages, 0x0012019F))"
Write-Host "GRANT mount-manager $([DeviceAccess]::Allow('\\.\MountPointManager', $false, $packages, 0x00100080))"
Write-Host "GRANT drive-root $([DeviceAccess]::Allow("$env:SystemDrive\", $true, $packages, 0x00100080))"

$sid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
& net session /delete /y 2>&1 | Out-Null
$sessions = ((& net session 2>&1 | Out-String).Trim() -replace '\s+', ' ')
if ($sessions -notmatch 'There are no entries') { throw "a server session is open before the rows: $sessions" }
Add-Content -Path $env:GITHUB_ENV -Value "CRAZYAGENT_FARM_SHARE_HOST=$ip"
Add-Content -Path $env:GITHUB_ENV -Value "CRAZYAGENT_FARM_USER_SID=$sid"
Write-Host "SETUP host=$ip user=$sid shares=$($names -join ',') git=[$(& git --version)]"
