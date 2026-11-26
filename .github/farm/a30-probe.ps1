param([string]$Root)
$ErrorActionPreference = 'Continue'

function Case([string]$Id, [scriptblock]$Body) {
  try {
    $r = (& $Body | Out-String).Trim() -replace "\r?\n", ' | '
    "CASE $Id : $r"
  } catch {
    "CASE $Id : ERROR $($_.Exception.Message)"
  }
}

function Version([string]$Prefix) {
  (Get-Content (Join-Path $Prefix 'node_modules\@openai\codex\package.json') -Raw | ConvertFrom-Json).version
}

function Leftovers([string]$Prefix) {
  $modules = Join-Path $Prefix 'node_modules'
  (Get-ChildItem $modules -Recurse -Depth 1 -Force -Directory | Where-Object { $_.Name.StartsWith('.') } | ForEach-Object { $_.FullName.Substring($modules.Length) }) -join ' '
}

New-Item -ItemType Directory -Force -Path $Root | Out-Null

$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = New-Object Security.Principal.WindowsPrincipal($identity)
Case 'P5-user' { "$($identity.Name) $($identity.User.Value)" }
Case 'P5-admin-role' { $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator) }
Case 'P5-integrity' { (whoami /groups | Select-String 'Mandatory Label').ToString().Trim() }
$file = Join-Path $Root 'owned.txt'
Set-Content -Path $file -Value 'x'
$folder = Join-Path $Root 'owned-dir'
New-Item -ItemType Directory -Force -Path $folder | Out-Null
Case 'P5-file-owner' { (Get-Acl $file).Owner }
Case 'P5-folder-owner' { (Get-Acl $folder).Owner }
Case 'P5-profile-owner' { (Get-Acl $env:USERPROFILE).Owner }
Case 'P5-root-volume' { (Get-Volume -DriveLetter ($Root.Substring(0, 1))).FileSystem }

$v = '26.13.1'
$zip = Join-Path $Root "node-v$v-win-x64.zip"
Case 'P4-download' { Invoke-WebRequest "https://nodejs.org/dist/v$v/node-v$v-win-x64.zip" -OutFile $zip -UseBasicParsing; (Get-Item $zip).Length }
Case 'P4-sha256' { (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower() }
Case 'P4-shasums' { (Invoke-WebRequest "https://nodejs.org/dist/v$v/SHASUMS256.txt" -UseBasicParsing).Content -split "`n" | Where-Object { $_ -match 'win-(x64|arm64)\.zip$' } }
$out = Join-Path $Root 'node-out'
New-Item -ItemType Directory -Force -Path $out | Out-Null
Case 'P4-tar' { & "$env:SystemRoot\System32\tar.exe" -xf $zip -C $out; "rc $LASTEXITCODE" }
$nodeHome = Join-Path $out "node-v$v-win-x64"
$node = Join-Path $nodeHome 'node.exe'
$npmCli = Join-Path $nodeHome 'node_modules\npm\bin\npm-cli.js'
Case 'P4-node-version' { & $node --version }
Case 'P4-npm-version' { & $node $npmCli --version }
Case 'P4-layout' { (Get-ChildItem $nodeHome -Name) -join ' ' }
Case 'P4-npm-cli' { Test-Path $npmCli }
Case 'P4-npx-cli' { Test-Path (Join-Path $nodeHome 'node_modules\npm\bin\npx-cli.js') }
Case 'P4-node-owner' { (Get-Acl $node).Owner }

$prefix = Join-Path $Root 'prefix'
New-Item -ItemType Directory -Force -Path $prefix | Out-Null
$env:npm_config_prefix = $prefix
$versions = & $node $npmCli view '@openai/codex' versions --json | ConvertFrom-Json
$stable = @($versions | Where-Object { $_ -match '^\d+\.\d+\.\d+$' })
$old = $stable[-2]
$new = $stable[-1]
Case 'P2-versions' { "$old -> $new" }
Case 'P2-install' { & $node $npmCli install -g "@openai/codex@$old" 2>&1 | Select-Object -Last 5; "rc $LASTEXITCODE" }
Case 'P2-prefix-top' { (Get-ChildItem $prefix -Force -Name) -join ' ' }
Case 'P2-has-bin' { Test-Path (Join-Path $prefix 'bin') }
Case 'P2-has-lib' { Test-Path (Join-Path $prefix 'lib') }
Case 'P2-modules' { (Get-ChildItem (Join-Path $prefix 'node_modules') -Recurse -Depth 1 -Directory -Name) -join ' ' }
Case 'P2-shim-text' { $t = [IO.File]::ReadAllText((Join-Path $prefix 'codex.cmd')); $t.Replace("`r", '\r').Replace("`n", '\n') }
Case 'P2-exes' { (Get-ChildItem $prefix -Recurse -Filter *.exe | ForEach-Object { $_.FullName.Substring($prefix.Length) }) -join ' ' }
Case 'P2-version-by-node' { & $node (Join-Path $prefix 'node_modules\@openai\codex\bin\codex.js') --version }
Case 'P2-installed' { Version $prefix }

$exe = Get-ChildItem $prefix -Recurse -Filter codex.exe | Select-Object -First 1
$start = New-Object Diagnostics.ProcessStartInfo $exe.FullName
$start.Arguments = 'app-server'
$start.UseShellExecute = $false
$start.RedirectStandardInput = $true
$start.RedirectStandardOutput = $true
$start.RedirectStandardError = $true
$running = [Diagnostics.Process]::Start($start)
Start-Sleep -Seconds 3
Case 'P3a-exe' { $exe.FullName.Substring($prefix.Length) }
Case 'P3a-running-before' { -not $running.HasExited }
Case 'P3a-update' { & $node $npmCli install -g "@openai/codex@$new" 2>&1 | Select-Object -Last 8; "rc $LASTEXITCODE" }
Case 'P3a-version-after' { Version $prefix }
Case 'P3a-exe-present' { Test-Path $exe.FullName }
Case 'P3a-leftovers' { Leftovers $prefix }
Case 'P3a-running-after' { -not $running.HasExited }
if (-not $running.HasExited) { $running.Kill($true) }

$prefix2 = Join-Path $Root 'prefix2'
New-Item -ItemType Directory -Force -Path $prefix2 | Out-Null
$env:npm_config_prefix = $prefix2
Case 'P3b-install' { & $node $npmCli install -g "@openai/codex@$old" 2>&1 | Select-Object -Last 3; "rc $LASTEXITCODE" }
$exe2 = Get-ChildItem $prefix2 -Recurse -Filter codex.exe | Select-Object -First 1
$held = [IO.File]::Open($exe2.FullName, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]'Read, Delete')
Case 'P3b-update-held' { & $node $npmCli install -g "@openai/codex@$new" 2>&1 | Select-Object -Last 8; "rc $LASTEXITCODE" }
Case 'P3b-version-after' { Version $prefix2 }
Case 'P3b-exe-present' { Test-Path $exe2.FullName }
Case 'P3b-leftovers' { Leftovers $prefix2 }
$held.Dispose()
Case 'P3b-update-released' { & $node $npmCli install -g "@openai/codex@$new" 2>&1 | Select-Object -Last 3; "rc $LASTEXITCODE" }
Case 'P3b-version-released' { Version $prefix2 }
Remove-Item Env:npm_config_prefix

Case 'P1-claude-install' { $script = Invoke-RestMethod 'https://claude.ai/install.ps1'; Invoke-Expression $script 2>&1 | Select-Object -Last 6 }
Case 'P1-claude-bin' { (Get-ChildItem "$env:USERPROFILE\.local\bin" -Force -Name) -join ' ' }
Case 'P1-claude-share' { (Get-ChildItem "$env:USERPROFILE\.local\share\claude" -Recurse -Depth 2 -Force -Name) -join ' ' }
Case 'P1-claude-file' { $c = "$env:USERPROFILE\.local\bin\claude.exe"; $i = Get-Item $c -Force; "$($i.LinkType) $($i.Target) $($i.Length)" }
Case 'P1-claude-version' { & "$env:USERPROFILE\.local\bin\claude.exe" --version }
'probe done'
