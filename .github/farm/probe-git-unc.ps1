$ErrorActionPreference = 'Continue'
Set-StrictMode -Version 2

$ws = $env:GITHUB_WORKSPACE
$helper = Join-Path $ws 'target\debug\crazyagent-win-boundary.exe'
if (-not (Test-Path -LiteralPath $helper)) { throw "the boundary helper is missing: $helper" }
$base = 'C:\probe'
$grants = "$base\grants"
$profiles = "$base\profiles"
$shares = "$base\shares"
foreach ($folder in @($base, $grants, $profiles, $shares, "$base\p1w", "$base\work", "$base\work\home", "$base\work\tmp", "$base\lat", "$base\load")) {
    New-Item -ItemType Directory -Force -Path $folder | Out-Null
}
$cmd = Join-Path $env:SystemRoot 'System32\cmd.exe'
$git = (Get-Command git -CommandType Application | Select-Object -First 1).Source
$ip = (Get-NetIPAddress -AddressFamily IPv4 | Where-Object { $_.IPAddress -notlike '127.*' -and $_.IPAddress -notlike '169.254.*' } | Select-Object -First 1).IPAddress
$hostName = $env:COMPUTERNAME
$os = (Get-CimInstance Win32_OperatingSystem).Caption
Write-Host "SETUP os=[$os] version=$([Environment]::OSVersion.Version) cores=$([Environment]::ProcessorCount) git=[$git] ip=$ip host=$hostName"
Write-Host "SETUP $(& $git --version)"
Write-Host "SETUP git-folder $((icacls (Split-Path (Split-Path $git)) | Out-String) -replace '\s+', ' ')"
Write-Host "SETUP lanmanserver=$((Get-Service LanmanServer).Status) lanmanworkstation=$((Get-Service LanmanWorkstation).Status)"
auditpol /set /subcategory:"Logon" /success:enable /failure:enable | Out-Null
Write-Host "SETUP audit $((auditpol /get /subcategory:'Logon' | Out-String) -replace '\s+', ' ')"
Write-Host "SETUP symlinks $((fsutil behavior query SymlinkEvaluation | Out-String) -replace '\s+', ' ')"
foreach ($share in 'pf', 'pa', 'pn', 'pi', 'pc', 'pl') {
    New-Item -ItemType Directory -Force -Path "$shares\$share" | Out-Null
    net share "$share=$shares\$share" "/GRANT:Everyone,READ" | Out-Null
}
Set-Content -NoNewline -Path "$shares\pf\f.txt" -Value 'probe-share-file'
Set-Content -NoNewline -Path "$shares\pl\exclude" -Value 'linked-exclude'
Write-Host "SETUP shares $((net share | Out-String) -replace '\s+', ' ')"

function Get-LastRecord {
    try { (Get-WinEvent -LogName Security -MaxEvents 1 -ErrorAction Stop).RecordId } catch { 0 }
}

function Get-NetworkLogons([long]$after) {
    $xpath = "*[System[(EventID=4624 or EventID=4625) and EventRecordID>$after]] and *[EventData[Data[@Name='LogonType']='3']]"
    try { @(Get-WinEvent -LogName Security -FilterXPath $xpath -ErrorAction Stop) } catch { @() }
}

function Format-Logon($logon) {
    $data = @{}
    foreach ($field in ([xml]$logon.ToXml()).Event.EventData.Data) { $data[$field.Name] = $field.'#text' }
    "event=$($logon.Id) user=$($data['TargetUserName']) package=$($data['AuthenticationPackageName']) lm=$($data['LmPackageName']) ip=$($data['IpAddress'])"
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

function Measure-Open([string]$phase, [string]$label, [scriptblock]$action, [switch]$KeepSessions) {
    if (-not $KeepSessions) { Reset-Sessions }
    $before = Get-LastRecord
    $watch = [Diagnostics.Stopwatch]::StartNew()
    $result = & $action
    $watch.Stop()
    Start-Sleep -Seconds 2
    $logons = @(Get-NetworkLogons $before)
    $credentialed = @($logons | Where-Object { (Format-Logon $_) -notmatch 'user=ANONYMOUS LOGON' })
    $sessions = Get-ServerSessions
    $hasSession = $sessions -notmatch 'There are no entries'
    $text = ($result.Output -replace '\s+', ' ').Trim()
    if ($text.Length -gt 300) { $text = $text.Substring(0, 300) }
    Write-Host "$phase $label exit=$($result.Exit) ms=$($watch.ElapsedMilliseconds) logons=$($logons.Count) credentialed=$($credentialed.Count) session=$hasSession out=[$text]"
    foreach ($logon in $logons) { Write-Host "$phase $label   logon $(Format-Logon $logon)" }
    Write-Host "$phase $label   server=[$sessions] client=[$(Get-ClientConnections)]"
    [pscustomobject]@{ Phase = $phase; Label = $label; Exit = $result.Exit; Logons = $logons.Count; Credentialed = $credentialed.Count; Session = $hasSession }
}

function Measure-Runs([int]$count, [scriptblock]$action) {
    $times = @()
    for ($run = 0; $run -lt $count; $run++) {
        $watch = [Diagnostics.Stopwatch]::StartNew()
        & $action | Out-Null
        $watch.Stop()
        $times += $watch.Elapsed.TotalMilliseconds
    }
    $sorted = @($times | Sort-Object)
    $p90 = $sorted[[Math]::Ceiling(0.9 * $count) - 1]
    "min=$([Math]::Round($sorted[0])) median=$([Math]::Round($sorted[[int][Math]::Floor($count / 2)])) p90=$([Math]::Round($p90)) max=$([Math]::Round($sorted[-1])) ms over $count runs"
}

function Get-Load {
    $compilers = @(Get-Process -Name rustc, cargo -ErrorAction SilentlyContinue).Count
    $cpu = (Get-CimInstance Win32_Processor | Measure-Object -Property LoadPercentage -Average).Average
    "compilers=$compilers cpu=$cpu%"
}

function New-Tree([string]$tree) {
    $bytes = [byte[]](0x61)
    for ($folder = 0; $folder -lt 1000; $folder++) {
        $path = "$tree\d$folder"
        [void][IO.Directory]::CreateDirectory($path)
        for ($file = 0; $file -lt 100; $file++) { [IO.File]::WriteAllBytes("$path\f$file.txt", $bytes) }
    }
}

function Measure-Grant([string]$condition, [string]$tree, [string]$container) {
    $boundary = Write-Profile "grant-$condition" $container @() @($tree)
    $loadBefore = Get-Load
    $watch = [Diagnostics.Stopwatch]::StartNew()
    $first = Invoke-Contained $boundary $cmd @('/d', '/c', 'exit 0')
    $watch.Stop()
    $firstMs = $watch.ElapsedMilliseconds
    $watch = [Diagnostics.Stopwatch]::StartNew()
    $second = Invoke-Contained $boundary $cmd @('/d', '/c', 'exit 0')
    $watch.Stop()
    $entries = @((icacls "$tree\d999\f99.txt" | Out-String) -split "`n" | Where-Object { $_ -match 'S-1-15-2-\d+-' }).Count
    Write-Host "P3 grant $condition files=100000 first-exit=$($first.Exit) first-ms=$firstMs second-exit=$($second.Exit) second-ms=$($watch.ElapsedMilliseconds) container-entries-on-last-file=$entries load-before=[$loadBefore] load-after=[$(Get-Load)] out=[$(($first.Output -replace '\s+', ' ').Trim())]"
}

$rows = @()

$p1Nocap = Write-Profile 'p1-nocap' 'crazyagent.probe-p1-nocap' @() @("$base\p1w")
$p1Net = Write-Profile 'p1-net' 'crazyagent.probe-p1-net' @('internetClient') @("$base\p1w")
$targets = [ordered]@{
    ip = "\\$ip\pf\f.txt"
    host = "\\$hostName\pf\f.txt"
    loopback = "\\127.0.0.1\pf\f.txt"
    adminshare = "\\localhost\C$\Windows\win.ini"
}
$nonLoopback = @('ip', 'host')
$rows += Measure-Open 'P1-launch' 'nocap' { Invoke-Contained $p1Nocap $cmd @('/d', '/c', 'echo launched') }
$rows += Measure-Open 'P1-launch' 'net' { Invoke-Contained $p1Net $cmd @('/d', '/c', 'echo launched') }
foreach ($name in $targets.Keys) {
    $target = $targets[$name]
    $rows += Measure-Open 'P1-nocap' $name { Invoke-Contained $p1Nocap $cmd @('/d', '/c', "type $target") }
}
foreach ($name in $targets.Keys) {
    $target = $targets[$name]
    $rows += Measure-Open 'P1-net' $name { Invoke-Contained $p1Net $cmd @('/d', '/c', "type $target") }
}
foreach ($name in $targets.Keys) {
    $target = $targets[$name]
    $rows += Measure-Open 'P1-owner' $name { Invoke-Owner $cmd @('/d', '/c', "type $target") }
}
$target = $targets['ip']
Reset-Sessions
Invoke-Owner $cmd @('/d', '/c', "type $target") | Out-Null
$rows += Measure-Open 'P1-owner-session' 'nocap-ip' { Invoke-Contained $p1Nocap $cmd @('/d', '/c', "type $target") } -KeepSessions
net use P: "\\$ip\pf" /persistent:no 2>&1 | Out-Null
Write-Host "P1 mapped $((net use | Out-String) -replace '\s+', ' ')"
$rows += Measure-Open 'P1-mapped' 'nocap' { Invoke-Contained $p1Nocap $cmd @('/d', '/c', 'type P:\f.txt') }
Reset-Sessions
Invoke-Owner $cmd @('/d', '/c', 'type P:\f.txt') | Out-Null
$rows += Measure-Open 'P1-mapped-session' 'nocap' { Invoke-Contained $p1Nocap $cmd @('/d', '/c', 'type P:\f.txt') } -KeepSessions
net use P: /delete /y 2>&1 | Out-Null

$env:GIT_CONFIG_NOSYSTEM = '1'
$env:GIT_CONFIG_GLOBAL = 'NUL'
$env:GIT_TERMINAL_PROMPT = '0'
$env:GIT_NO_LAZY_FETCH = '1'
$env:HOME = "$base\work\home"
$env:TEMP = "$base\work\tmp"
$env:TMP = "$base\work\tmp"
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
$rows += Measure-Open 'P2-first-launch' 'grant-over-planted-tree' { Invoke-Contained $gitProfile $git @('--version') }
foreach ($item in $items.Keys) {
    $repo = "$base\work\r-$item"
    $bundle = "$base\work\b-$item-contained.bundle"
    $rows += Measure-Open 'P2-nocap' "$item-ls-files" { Invoke-Contained $gitProfile $git @('-C', $repo, '--no-optional-locks', 'ls-files', '--cached', '--others', '--exclude-standard') }
    $rows += Measure-Open 'P2-nocap' "$item-add" { Invoke-Contained $gitProfile $git @('-C', $repo, 'add', '-A') }
    $rows += Measure-Open 'P2-nocap' "$item-commit" { Invoke-Contained $gitProfile $git @('-C', $repo, '-c', 'user.name=probe', '-c', 'user.email=probe@example.invalid', 'commit', '-q', '--allow-empty', '-m', 'contained') }
    $rows += Measure-Open 'P2-nocap' "$item-bundle" { Invoke-Contained $gitProfile $git @('-C', $repo, 'bundle', 'create', $bundle, 'HEAD') }
}
foreach ($item in $items.Keys) {
    $repo = "$base\work\r-$item"
    $bundle = "$base\work\b-$item-owner.bundle"
    $rows += Measure-Open 'P2-owner' "$item-ls-files" { Invoke-Owner $git @('-C', $repo, '--no-optional-locks', 'ls-files', '--cached', '--others', '--exclude-standard') }
    $rows += Measure-Open 'P2-owner' "$item-add" { Invoke-Owner $git @('-C', $repo, 'add', '-A') }
    $rows += Measure-Open 'P2-owner' "$item-commit" { Invoke-Owner $git @('-C', $repo, '-c', 'user.name=probe', '-c', 'user.email=probe@example.invalid', 'commit', '-q', '--allow-empty', '-m', 'owner') }
    $rows += Measure-Open 'P2-owner' "$item-bundle" { Invoke-Owner $git @('-C', $repo, 'bundle', 'create', $bundle, 'HEAD') }
}

$watch = [Diagnostics.Stopwatch]::StartNew()
New-Tree "$base\grant-quiet"
New-Tree "$base\grant-loaded"
$watch.Stop()
Write-Host "P3 trees made: 2 x 100000 files in $([Math]::Round($watch.Elapsed.TotalSeconds)) s"
& $git clone -q $ws "$base\lat\repo"
$latRepo = "$base\lat\repo"
$latFiles = @(& $git -C $latRepo ls-files).Count
$latProfile = Write-Profile 'p3-lat' 'crazyagent.probe-lat' @() @("$base\lat")
$watch = [Diagnostics.Stopwatch]::StartNew()
$latFirst = Invoke-Contained $latProfile $cmd @('/d', '/c', 'exit 0')
$watch.Stop()
Write-Host "P3 grant lat-repo files=$latFiles first-exit=$($latFirst.Exit) first-ms=$($watch.ElapsedMilliseconds)"
$listArgs = @('-C', $latRepo, '--no-optional-locks', 'ls-files', '-z', '--cached', '--others', '--exclude-standard')

Measure-Grant 'quiet' "$base\grant-quiet" 'crazyagent.probe-grant-quiet'
Write-Host "P3 latency quiet direct $(Measure-Runs 20 { & $git @listArgs }) load=[$(Get-Load)]"
Write-Host "P3 latency quiet contained $(Measure-Runs 20 { & $helper '--profile' $latProfile '--' $git @listArgs }) load=[$(Get-Load)]"

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
Measure-Grant 'loaded' "$base\grant-loaded" 'crazyagent.probe-grant-loaded'
Write-Host "P3 latency loaded direct $(Measure-Runs 20 { & $git @listArgs }) load=[$(Get-Load)]"
Write-Host "P3 latency loaded contained $(Measure-Runs 20 { & $helper '--profile' $latProfile '--' $git @listArgs }) load=[$(Get-Load)]"
New-Item -ItemType File -Force -Path "$base\load\stop" | Out-Null
Get-Process -Name cargo, rustc -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
$load | Stop-Process -Force -ErrorAction SilentlyContinue
Write-Host "P3 load rounds started: $(@(Get-ChildItem "$base\load" -Filter 'log*.txt').Count)"

$p1 = @($rows | Where-Object { $_.Phase -eq 'P1-nocap' -and $nonLoopback -contains $_.Label })
$opened = @($p1 | Where-Object { $_.Exit -eq 0 }).Count
$logged = @($p1 | Where-Object { $_.Logons -gt 0 }).Count
$sessioned = @($p1 | Where-Object { $_.Session }).Count
$control = @($rows | Where-Object { $_.Phase -eq 'P1-owner' -and $nonLoopback -contains $_.Label -and $_.Exit -eq 0 -and ($_.Logons -gt 0 -or $_.Session) })
$controlValid = $control.Count -eq $nonLoopback.Count
$launched = @($rows | Where-Object { $_.Phase -eq 'P1-launch' -and $_.Exit -eq 0 }).Count -eq 2
Write-Host "P1 CONTROL containers launched=$launched owner non-loopback opens logged a logon or session=$controlValid"
Write-Host "P1 VERDICT nocap non-loopback names: opened=$opened logons=$logged sessions=$sessioned A-holds=$($launched -and $controlValid -and $opened -eq 0 -and $logged -eq 0 -and $sessioned -eq 0)"
foreach ($phase in 'P1-net', 'P1-owner-session', 'P1-mapped', 'P1-mapped-session') {
    foreach ($row in @($rows | Where-Object { $_.Phase -eq $phase })) {
        Write-Host "P1 SUMMARY $phase $($row.Label) exit=$($row.Exit) logons=$($row.Logons) credentialed=$($row.Credentialed) session=$($row.Session)"
    }
}
foreach ($row in @($rows | Where-Object { $_.Phase -like 'P2-*' })) {
    Write-Host "P2 SUMMARY $($row.Phase) $($row.Label) exit=$($row.Exit) logons=$($row.Logons) credentialed=$($row.Credentialed) session=$($row.Session)"
}
$containedHits = @($rows | Where-Object { ($_.Phase -eq 'P2-nocap' -or $_.Phase -eq 'P2-first-launch') -and ($_.Logons -gt 0 -or $_.Session) }).Count
$ownerHits = @($rows | Where-Object { $_.Phase -eq 'P2-owner' -and ($_.Logons -gt 0 -or $_.Session) }).Count
$gitLaunched = @($rows | Where-Object { $_.Phase -eq 'P2-first-launch' -and $_.Exit -eq 0 }).Count -eq 1
Write-Host "P2 VERDICT git-launched-in-container=$gitLaunched contained runs that reached a share=$containedHits owner runs that reached a share=$ownerHits"
exit 0
