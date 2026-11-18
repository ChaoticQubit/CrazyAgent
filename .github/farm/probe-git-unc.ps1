$ErrorActionPreference = 'Continue'
Set-StrictMode -Version 2

$ws = $env:GITHUB_WORKSPACE
$helper = Join-Path $ws 'target\debug\crazyagent-win-boundary.exe'
if (-not (Test-Path -LiteralPath $helper)) { throw "the boundary helper is missing: $helper" }
$base = 'C:\probe'
$grants = "$base\grants"
$profiles = "$base\profiles"
$shares = "$base\shares"
foreach ($folder in @($base, $grants, $profiles, $shares, "$base\io", "$base\p1w", "$base\work", "$base\work\home", "$base\work\tmp", "$base\lat", "$base\load")) {
    New-Item -ItemType Directory -Force -Path $folder | Out-Null
}
$stdin = "$base\io\stdin-empty"
New-Item -ItemType File -Force -Path $stdin | Out-Null
$cmd = Join-Path $env:SystemRoot 'System32\cmd.exe'
$git = (Get-Command git -CommandType Application | Select-Object -First 1).Source
$ip = (Get-NetIPAddress -AddressFamily IPv4 | Where-Object { $_.IPAddress -notlike '127.*' -and $_.IPAddress -notlike '169.254.*' } | Select-Object -First 1).IPAddress
$hostName = $env:COMPUTERNAME
$os = (Get-CimInstance Win32_OperatingSystem).Caption
Write-Host "SETUP os=[$os] version=$([Environment]::OSVersion.Version) cores=$([Environment]::ProcessorCount) git=[$git] ip=$ip host=$hostName"
Write-Host "SETUP $(& $git --version)"
Write-Host "SETUP lanmanserver=$((Get-Service LanmanServer).Status) lanmanworkstation=$((Get-Service LanmanWorkstation).Status)"
auditpol /set /subcategory:"Logon" /success:enable /failure:enable | Out-Null
Write-Host "SETUP audit $((auditpol /get /subcategory:'Logon' | Out-String) -replace '\s+', ' ')"
foreach ($share in 'pf', 'pa', 'pn', 'pi', 'pc', 'pl') {
    New-Item -ItemType Directory -Force -Path "$shares\$share" | Out-Null
    net share "$share=$shares\$share" "/GRANT:Everyone,READ" | Out-Null
}
Set-Content -NoNewline -Path "$shares\pf\f.txt" -Value 'probe-share-file'
Set-Content -NoNewline -Path "$shares\pl\exclude" -Value 'linked-exclude'

function Get-LastRecord {
    try { (Get-WinEvent -LogName Security -MaxEvents 1 -ErrorAction Stop).RecordId } catch { 0 }
}

function Get-Field($logged, [string]$name) {
    $value = ''
    foreach ($field in ([xml]$logged.ToXml()).Event.EventData.Data) {
        if ($field.Name -eq $name) { $value = $field.'#text' }
    }
    $value
}

function Get-SecurityEvents([long]$after) {
    try {
        @(Get-WinEvent -LogName Security -FilterXPath "*[System[EventRecordID>$after]]" -ErrorAction Stop)
    } catch {
        if ($_.Exception.Message -notlike '*No events were found*') { Write-Host "EVENTS query failed: $($_.Exception.Message)" }
        @()
    }
}

function Format-Logon($logged) {
    "event=$($logged.Id) type=$(Get-Field $logged 'LogonType') user=$(Get-Field $logged 'TargetUserName') package=$(Get-Field $logged 'AuthenticationPackageName') lm=$(Get-Field $logged 'LmPackageName') ip=$(Get-Field $logged 'IpAddress')"
}

function Reset-Sessions {
    net session /delete /y 2>&1 | Out-Null
    Start-Sleep -Milliseconds 500
}

function Get-ServerSessions {
    ((net session 2>&1 | Out-String).Trim() -replace '\s+', ' ')
}

function Get-ClientConnections {
    try { ((Get-SmbConnection -ErrorAction Stop | ForEach-Object { "$($_.ServerName)\$($_.ShareName)" }) -join ',') } catch { 'unread' }
}

function Write-Profile([string]$name, [string]$container, [string[]]$capabilities, [string[]]$writes) {
    $lines = @('crazyagent-win-boundary 2', "container $container", "grants $grants")
    foreach ($capability in $capabilities) { $lines += "capability $capability" }
    foreach ($write in $writes) { $lines += "write $write" }
    $lines += 'processes 64'
    $path = Join-Path $profiles "$name.sb"
    [IO.File]::WriteAllText($path, (($lines -join "`n") + "`n"))
    $path
}

function Invoke-Contained([string]$boundary, [string]$program, [string[]]$arguments) {
    $output = & $helper '--profile' $boundary '--' $program @arguments 2>&1
    [pscustomobject]@{ Exit = $LASTEXITCODE; Output = ($output | Out-String) }
}

function Invoke-Owner([string]$program, [string[]]$arguments) {
    $output = & $program @arguments 2>&1
    [pscustomobject]@{ Exit = $LASTEXITCODE; Output = ($output | Out-String) }
}

function Format-Argument([string]$value) {
    if ($value -match '[\s"]') { '"' + ($value -replace '"', '\"') + '"' } else { $value }
}

function Invoke-Redirected([string]$program, [string[]]$arguments, [string]$source) {
    $out = "$base\io\out.txt"
    $err = "$base\io\err.txt"
    $line = ($arguments | ForEach-Object { Format-Argument $_ }) -join ' '
    try {
        $process = Start-Process -FilePath $program -ArgumentList $line -RedirectStandardInput $source -RedirectStandardOutput $out -RedirectStandardError $err -NoNewWindow -Wait -PassThru -ErrorAction Stop
        [pscustomobject]@{ Exit = $process.ExitCode; Output = ("$(Get-Content -Raw -LiteralPath $out) $(Get-Content -Raw -LiteralPath $err)") }
    } catch {
        [pscustomobject]@{ Exit = -1; Output = "start failed: $($_.Exception.Message)" }
    }
}

function Invoke-ContainedGit([string]$boundary, [string[]]$arguments) {
    Invoke-Redirected $helper (@('--profile', $boundary, '--', $git) + $arguments) $stdin
}

function Invoke-OwnerGit([string[]]$arguments) {
    Invoke-Redirected $git $arguments $stdin
}

function Measure-Open([string]$phase, [string]$label, [scriptblock]$action, [switch]$KeepSessions) {
    if (-not $KeepSessions) { Reset-Sessions }
    $before = Get-LastRecord
    $watch = [Diagnostics.Stopwatch]::StartNew()
    $result = & $action
    $watch.Stop()
    Start-Sleep -Seconds 2
    $events = @(Get-SecurityEvents $before)
    $logons = @($events | Where-Object { ($_.Id -eq 4624 -or $_.Id -eq 4625) -and (Get-Field $_ 'LogonType') -eq '3' })
    $failed = @($logons | Where-Object { $_.Id -eq 4625 })
    $ids = (($events | Group-Object Id | ForEach-Object { "$($_.Name)x$($_.Count)" }) -join ',')
    $sessions = Get-ServerSessions
    $hasSession = $sessions -notmatch 'There are no entries'
    $text = ($result.Output -replace '\s+', ' ').Trim()
    if ($text.Length -gt 300) { $text = $text.Substring(0, 300) }
    Write-Host "$phase $label exit=$($result.Exit) ms=$($watch.ElapsedMilliseconds) logons=$($logons.Count) failed=$($failed.Count) session=$hasSession events=[$ids] out=[$text]"
    foreach ($logon in $logons) { Write-Host "$phase $label   logon $(Format-Logon $logon)" }
    Write-Host "$phase $label   server=[$sessions] client=[$(Get-ClientConnections)]"
    [pscustomobject]@{ Phase = $phase; Label = $label; Exit = $result.Exit; Logons = $logons.Count; Failed = $failed.Count; Session = $hasSession; Output = $text }
}

function Measure-Runs([int]$count, [scriptblock]$action) {
    $times = @()
    $failures = 0
    for ($run = 0; $run -lt $count; $run++) {
        $watch = [Diagnostics.Stopwatch]::StartNew()
        $result = & $action
        $watch.Stop()
        if ($result.Exit -ne 0) { $failures++ }
        $times += $watch.Elapsed.TotalMilliseconds
    }
    $sorted = @($times | Sort-Object)
    $p90 = $sorted[[Math]::Ceiling(0.9 * $count) - 1]
    "min=$([Math]::Round($sorted[0])) median=$([Math]::Round($sorted[[int][Math]::Floor($count / 2)])) p90=$([Math]::Round($p90)) max=$([Math]::Round($sorted[-1])) ms over $count runs, failed=$failures"
}

function Get-Load {
    $compilers = @(Get-Process -Name rustc, cargo -ErrorAction SilentlyContinue).Count
    $cpu = (Get-CimInstance Win32_Processor | Measure-Object -Property LoadPercentage -Average).Average
    "compilers=$compilers cpu=$cpu%"
}

$rows = @()


$env:GIT_CONFIG_NOSYSTEM = '1'
$env:GIT_ATTR_NOSYSTEM = '1'
$env:GIT_TERMINAL_PROMPT = '0'
$env:GIT_NO_LAZY_FETCH = '1'
$env:HOME = "$base\work\home"
$env:TEMP = "$base\work\tmp"
$env:TMP = "$base\work\tmp"
$none = ([Environment]::SystemDirectory -replace '\\', '/') + '/crazyagent-none'
$safe = @('-c', "core.hooksPath=$none", '-c', 'core.fsmonitor=false', '-c', 'core.pager=cat', '-c', 'submodule.recurse=false', '-c', 'protocol.allow=never', '-c', "gpg.program=$none", '-c', "gpg.x509.program=$none", '-c', "gpg.ssh.program=$none", '-c', 'log.showSignature=false')
$emptyConfig = "$base\work\home\empty-config"
New-Item -ItemType File -Force -Path $emptyConfig | Out-Null
$script:excludes = ''

function Set-Variant([string]$value) {
    $env:GIT_CONFIG_GLOBAL = $value
    $script:excludes = "core.excludesFile=$value"
}

function Get-GitArgs([string]$repo, [string[]]$command) {
    @('-C', $repo, '--no-optional-locks') + $safe + @('-c', $script:excludes) + $command
}

function Get-StepArgs([string]$step, [string]$bundle) {
    switch ($step) {
        'ls-files' { @('ls-files', '--cached', '--others', '--exclude-standard') }
        'add' { @('add', '-A') }
        'commit' { @('-c', 'user.name=probe', '-c', 'user.email=probe@example.invalid', 'commit', '-q', '--allow-empty', '--no-gpg-sign', '-m', 'probe') }
        'bundle' { @('bundle', 'create', $bundle, 'HEAD') }
    }
}

$steps = @('ls-files', 'add', 'commit', 'bundle')
$env:GIT_CONFIG_GLOBAL = 'NUL'
$clean = "$base\work\r-clean"
& $git init -q $clean
Set-Content -Path "$clean\a.txt" -Value 'a'
& $git -C $clean add a.txt
& $git -C $clean -c user.name=probe -c user.email=probe@example.invalid commit -q -m init
Set-Content -Path "$clean\b.txt" -Value 'b'
$items = [ordered]@{ alternates = 'pa'; nested = 'pn'; include = 'pi'; commondir = 'pc'; excludelink = 'pl' }
foreach ($item in $items.Keys) {
    $repo = "$base\work\r-$item"
    & $git init -q $repo
    Set-Content -Path "$repo\a.txt" -Value 'a'
    & $git -C $repo add a.txt
    & $git -C $repo -c user.name=probe -c user.email=probe@example.invalid commit -q -m init
}
Set-Content -NoNewline -Path "$base\work\r-alternates\.git\objects\info\alternates" -Value "\\$ip\pa\objects`n"
New-Item -ItemType Directory -Force -Path "$base\work\r-nested\sub" | Out-Null
Set-Content -Path "$base\work\r-nested\sub\x.txt" -Value 'x'
Set-Content -NoNewline -Path "$base\work\r-nested\sub\.git" -Value "gitdir: \\$ip\pn\nested`n"
Add-Content -Path "$base\work\r-include\.git\config" -Value "[include]`n`tpath = //$ip/pi/cfg"
Set-Content -NoNewline -Path "$base\work\r-commondir\.git\commondir" -Value "\\$ip\pc\common`n"
Remove-Item -LiteralPath "$base\work\r-excludelink\.git\info\exclude" -Force
Write-Host "P2 link $((& $cmd /d /c mklink "$base\work\r-excludelink\.git\info\exclude" "\\$ip\pl\exclude" 2>&1 | Out-String) -replace '\s+', ' ')"
$gitProfile = Write-Profile 'p2-git' 'crazyagent.probe-git' @() @("$base\work")
$rows += Measure-Open 'P2-first-launch' 'grant-over-planted-tree' { Invoke-ContainedGit $gitProfile @('--version') }
Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Security.AccessControl;
using System.Security.Principal;
using Microsoft.Win32.SafeHandles;
public static class NulAccess {
    [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    static extern SafeFileHandle CreateFileW(string name, uint access, uint share, IntPtr attributes, uint disposition, uint flags, IntPtr template);
    [DllImport("advapi32.dll", SetLastError = true)]
    static extern bool GetKernelObjectSecurity(SafeFileHandle handle, uint information, byte[] descriptor, uint length, out uint needed);
    [DllImport("advapi32.dll", SetLastError = true)]
    static extern bool SetKernelObjectSecurity(SafeFileHandle handle, uint information, byte[] descriptor);
    const uint ReadControl = 0x20000;
    const uint WriteDac = 0x40000;
    const uint Dacl = 4;
    static SafeFileHandle Open() {
        SafeFileHandle handle = CreateFileW(@"\\.\NUL", ReadControl | WriteDac, 7, IntPtr.Zero, 3, 0, IntPtr.Zero);
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
    public static string Sddl() {
        using (SafeFileHandle handle = Open()) { return Read(handle).GetSddlForm(AccessControlSections.Access); }
    }
    public static string Grant(int mask) {
        using (SafeFileHandle handle = Open()) {
            RawSecurityDescriptor descriptor = Read(handle);
            SecurityIdentifier packages = new SecurityIdentifier("S-1-15-2-1");
            RawAcl list = descriptor.DiscretionaryAcl;
            for (int at = list.Count - 1; at >= 0; at--) {
                CommonAce entry = list[at] as CommonAce;
                if (entry != null && entry.SecurityIdentifier == packages) { list.RemoveAce(at); }
            }
            list.InsertAce(list.Count, new CommonAce(AceFlags.None, AceQualifier.AccessAllowed, mask, packages, false, null));
            descriptor.DiscretionaryAcl = list;
            byte[] bytes = new byte[descriptor.BinaryLength];
            descriptor.GetBinaryForm(bytes, 0);
            if (!SetKernelObjectSecurity(handle, Dacl, bytes)) { throw new Win32Exception(Marshal.GetLastWin32Error()); }
            return Read(handle).GetSddlForm(AccessControlSections.Access);
        }
    }
}
'@

function Test-Nul([string]$stage, [string]$boundary) {
    $read = Invoke-Contained $boundary $cmd @('/d', '/c', 'type NUL')
    $write = Invoke-Contained $boundary $cmd @('/d', '/c', 'echo x>NUL')
    $version = Invoke-ContainedGit $boundary @('--version')
    Write-Host "NUL $stage read-exit=$($read.Exit) [$(($read.Output -replace '\s+', ' ').Trim())] write-exit=$($write.Exit) [$(($write.Output -replace '\s+', ' ').Trim())] git-version-exit=$($version.Exit) [$(($version.Output -replace '\s+', ' ').Trim())]"
}

$netProfile = Write-Profile 'p2-net' 'crazyagent.probe-git-net' @('internetClient') @("$base\p1w")
Write-Host "NUL dacl-before $([NulAccess]::Sddl())"
Test-Nul 'before nocap' $gitProfile
Write-Host "NUL dacl-read $([NulAccess]::Grant(0x120089))"
Test-Nul 'read-only nocap' $gitProfile
Write-Host "NUL dacl-readwrite $([NulAccess]::Grant(0x12019F))"
Test-Nul 'read-write nocap' $gitProfile
Test-Nul 'read-write internetClient' $netProfile

$chosen = $null
foreach ($variant in @($emptyConfig, 'NUL')) {
    Set-Variant $variant
    $tag = if ($variant -eq 'NUL') { 'nul' } else { 'file' }
    $results = @{}
    foreach ($step in $steps) {
        $arguments = Get-GitArgs $clean (Get-StepArgs $step "$base\work\b-clean-$tag.bundle")
        $row = Measure-Open "P2-clean-$tag" $step { Invoke-ContainedGit $gitProfile $arguments }
        $rows += $row
        $results[$step] = $row
    }
    $listed = $results['ls-files'].Exit -eq 0 -and $results['ls-files'].Output -match 'b\.txt'
    $made = @($steps | Where-Object { $results[$_].Exit -ne 0 }).Count -eq 0
    Write-Host "P2 CLEAN variant=$tag lists-untracked=$listed all-steps-exit-0=$made"
    if ($null -eq $chosen -and $listed -and $made) { $chosen = $variant }
}
$gitWorks = $null -ne $chosen
if ($gitWorks) {
    Set-Variant $chosen
    Write-Host "P2 using GIT_CONFIG_GLOBAL and core.excludesFile = $chosen"
    foreach ($item in $items.Keys) {
        $repo = "$base\work\r-$item"
        foreach ($step in $steps) {
            $arguments = Get-GitArgs $repo (Get-StepArgs $step "$base\work\b-$item-contained.bundle")
            $rows += Measure-Open 'P2-nocap' "$item-$step" { Invoke-ContainedGit $gitProfile $arguments }
        }
    }
    foreach ($item in $items.Keys) {
        $repo = "$base\work\r-$item"
        foreach ($step in $steps) {
            $arguments = Get-GitArgs $repo (Get-StepArgs $step "$base\work\b-$item-owner.bundle")
            $rows += Measure-Open 'P2-owner' "$item-$step" { Invoke-OwnerGit $arguments }
        }
    }
} else {
    Set-Variant $emptyConfig
}

& $git clone -q $ws "$base\lat\repo"
$latRepo = "$base\lat\repo"
$latFiles = @(& $git -C $latRepo ls-files).Count
$latProfile = Write-Profile 'p3-lat' 'crazyagent.probe-lat' @() @("$base\lat")
$latArgs = Get-GitArgs $latRepo (Get-StepArgs 'ls-files' '')
$probe = Invoke-ContainedGit $latProfile $latArgs
$probeLines = @(($probe.Output -split "`n") | Where-Object { $_.Trim() -ne '' }).Count
Write-Host "P3 contained list check exit=$($probe.Exit) lines=$probeLines tracked=$latFiles excludes=[$($script:excludes)]"
Write-Host "P3 latency quiet direct $(Measure-Runs 20 { Invoke-OwnerGit $latArgs }) load=[$(Get-Load)]"
Write-Host "P3 latency quiet contained $(Measure-Runs 20 { Invoke-ContainedGit $latProfile $latArgs }) load=[$(Get-Load)]"
Write-Host "P3 latency quiet direct-call $(Measure-Runs 20 { Invoke-Owner $git $latArgs }) load=[$(Get-Load)]"
Write-Host "P3 latency quiet contained-call $(Measure-Runs 20 { Invoke-Contained $latProfile $git $latArgs }) load=[$(Get-Load)]"

$loop = "$base\load\loop.ps1"
Set-Content -Path $loop -Value @'
param($workspace, $root)
Set-Location $workspace
$round = 0
while (-not (Test-Path "$root\load\stop")) {
    $round++
    & cargo build -p crazyagent-win-sandbox --all-targets --release --locked --target-dir "$root\load\t$round" *> "$root\load\log$round.txt"
}
'@
$load = Start-Process pwsh -ArgumentList @('-NoProfile', '-File', $loop, $ws, $base) -PassThru -WindowStyle Hidden
$waited = 0
while ($waited -lt 240 -and @(Get-Process -Name rustc -ErrorAction SilentlyContinue).Count -eq 0) { Start-Sleep -Seconds 2; $waited += 2 }
Start-Sleep -Seconds 20
Write-Host "P3 loaded start after $waited s: [$(Get-Load)]"
Write-Host "P3 latency loaded direct $(Measure-Runs 20 { Invoke-OwnerGit $latArgs }) load=[$(Get-Load)]"
Write-Host "P3 latency loaded contained $(Measure-Runs 20 { Invoke-ContainedGit $latProfile $latArgs }) load=[$(Get-Load)]"
Write-Host "P3 latency loaded direct-call $(Measure-Runs 20 { Invoke-Owner $git $latArgs }) load=[$(Get-Load)]"
Write-Host "P3 latency loaded contained-call $(Measure-Runs 20 { Invoke-Contained $latProfile $git $latArgs }) load=[$(Get-Load)]"
New-Item -ItemType File -Force -Path "$base\load\stop" | Out-Null
Get-Process -Name cargo, rustc -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
$load | Stop-Process -Force -ErrorAction SilentlyContinue

foreach ($row in @($rows | Where-Object { $_.Phase -like 'P2-*' })) {
    Write-Host "P2 SUMMARY $($row.Phase) $($row.Label) exit=$($row.Exit) logons=$($row.Logons) failed=$($row.Failed) session=$($row.Session)"
}
$containedHits = @($rows | Where-Object { ($_.Phase -eq 'P2-nocap' -or $_.Phase -eq 'P2-first-launch' -or $_.Phase -like 'P2-clean-*') -and ($_.Logons -gt 0 -or $_.Session) }).Count
$ownerHits = @($rows | Where-Object { $_.Phase -eq 'P2-owner' -and ($_.Logons -gt 0 -or $_.Session) }).Count
Write-Host "P2 VERDICT git-works-in-container=$gitWorks variant=[$chosen] contained runs that reached a share=$containedHits owner runs that reached a share=$ownerHits"
exit 0
