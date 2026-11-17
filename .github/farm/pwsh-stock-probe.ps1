$ErrorActionPreference = 'Stop'
$shell = Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0'
$kept = @('PATH', 'PATHEXT', 'SystemRoot', 'SystemDrive', 'windir', 'ComSpec', 'USERPROFILE', 'USERNAME', 'APPDATA', 'LOCALAPPDATA', 'TEMP', 'TMP')
$stock = @('Microsoft.PowerShell.Operation.Validation', 'PackageManagement', 'Pester', 'PowerShellGet', 'PSReadline', 'PSReadLine')
$script = "Set-Content -LiteralPath started.txt -Value 1; `$paths = `$env:PSModulePath -split ';' | Where-Object { `$_ }; Set-Content -LiteralPath paths.txt -Value ('entries=' + `$paths.Count + ' system32=' + [bool](`$paths -match 'system32') + ' programfiles=' + [bool](`$paths -match 'Program Files') + ' other=' + @(`$paths | Where-Object { `$_ -notmatch 'system32|Program Files' }).Count)"
function New-Root {
  $root = Join-Path ([IO.Path]::GetTempPath()) ('stock-' + [guid]::NewGuid().ToString('N'))
  New-Item -ItemType Directory $root | Out-Null
  return $root
}
function Measure-Start([string]$label, [hashtable]$set, [string]$profileRoot) {
  $root = New-Root
  $start = [Diagnostics.ProcessStartInfo]::new((Join-Path $shell 'powershell.exe'))
  foreach ($word in @('-NoProfile', '-NonInteractive', '-Command', $script)) { $start.ArgumentList.Add($word) }
  $start.Environment.Clear()
  foreach ($name in $kept) {
    $value = [Environment]::GetEnvironmentVariable($name)
    if ($value) { $start.Environment[$name] = $value }
  }
  foreach ($name in $set.Keys) { $start.Environment[$name] = $set[$name] }
  if ($profileRoot) {
    $start.Environment['USERPROFILE'] = $profileRoot
    $start.Environment['APPDATA'] = Join-Path $profileRoot 'AppData\Roaming'
    $start.Environment['LOCALAPPDATA'] = Join-Path $profileRoot 'AppData\Local'
  }
  $start.UseShellExecute = $false
  $start.CreateNoWindow = $true
  $start.WorkingDirectory = $root
  $start.RedirectStandardOutput = $true
  $start.RedirectStandardError = $true
  $clock = [Diagnostics.Stopwatch]::StartNew()
  $child = [Diagnostics.Process]::Start($start)
  $null = $child.StandardOutput.ReadToEndAsync()
  $null = $child.StandardError.ReadToEndAsync()
  $seconds = 'none'
  while ($clock.Elapsed.TotalSeconds -lt 90) {
    if (Test-Path -LiteralPath (Join-Path $root 'started.txt')) { $seconds = [math]::Round($clock.Elapsed.TotalSeconds, 2); break }
    if ($child.HasExited) { break }
    Start-Sleep -Milliseconds 50
  }
  if (-not $child.WaitForExit(30000)) { $child.Kill($true) }
  $paths = Join-Path $root 'paths.txt'
  $seen = if (Test-Path -LiteralPath $paths) { (Get-Content -LiteralPath $paths -Raw).Trim() } else { 'no paths.txt' }
  Write-Host "$label -> $seconds s ($seen)"
  return $seconds
}
$lines = @('| run | first statement at (s), each start |', '| --- | --- |')
$lines += "| image modules, cleared env (control) | $((1..2 | ForEach-Object { Measure-Start 'control' @{} '' }) -join ', ') |"
$folder = Join-Path $env:ProgramFiles 'WindowsPowerShell\Modules'
$aside = Join-Path $env:RUNNER_TEMP 'modules-aside'
New-Item -ItemType Directory -Force $aside | Out-Null
$extra = @(Get-ChildItem -LiteralPath $folder -Directory | Where-Object { $stock -notcontains $_.Name })
$moved = 0
foreach ($module in $extra) {
  try { Move-Item -LiteralPath $module.FullName -Destination $aside -ErrorAction Stop; $moved++ }
  catch { Write-Host "could not move $($module.Name): $($_.Exception.GetType().Name)" }
}
$left = @(Get-ChildItem -LiteralPath $folder -Directory | ForEach-Object Name) -join ', '
Write-Host "moved $moved of $($extra.Count) image module folders out of Program Files; left: $left"
$stockPath = @{ 'PSModulePath' = (Join-Path $shell 'Modules') }
$kept1 = New-Root
$lines += "| stock modules, cleared env, one profile kept across starts | $((1..5 | ForEach-Object { Measure-Start 'stock' $stockPath $kept1 }) -join ', ') |"
$lines += "| stock modules, cleared env, fresh profile each start | $((1..5 | ForEach-Object { Measure-Start 'stock-fresh' $stockPath (New-Root) }) -join ', ') |"
$kept2 = New-Root
$lines += "| stock modules, cleared env, no PSModulePath, one profile kept | $((1..5 | ForEach-Object { Measure-Start 'stock-nopath' @{} $kept2 }) -join ', ') |"
$lines += "| image module folders moved aside | $moved of $($extra.Count) |"
$lines | Out-File -Append -Encoding utf8 $env:GITHUB_STEP_SUMMARY
$lines | ForEach-Object { Write-Host $_ }
