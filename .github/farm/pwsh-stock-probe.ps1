$ErrorActionPreference = 'Stop'
$shell = Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0'
$kept = @('PATH', 'PATHEXT', 'SystemRoot', 'SystemDrive', 'windir', 'ComSpec', 'USERPROFILE', 'USERNAME', 'APPDATA', 'LOCALAPPDATA', 'TEMP', 'TMP')
$stock = @('Microsoft.PowerShell.Operation.Validation', 'PackageManagement', 'Pester', 'PowerShellGet', 'PSReadline', 'PSReadLine')
$script = "Set-Content -LiteralPath started.txt -Value 1; `$paths = `$env:PSModulePath -split ';' | Where-Object { `$_ }; Set-Content -LiteralPath paths.txt -Value ('entries=' + `$paths.Count + ' system32=' + [bool](`$paths -match 'system32') + ' programfiles=' + [bool](`$paths -match 'Program Files') + ' other=' + @(`$paths | Where-Object { `$_ -notmatch 'system32|Program Files' }).Count)"
function Measure-Start([string]$label, [hashtable]$set, [bool]$freshProfile) {
  $root = Join-Path ([IO.Path]::GetTempPath()) ('stock-' + [guid]::NewGuid().ToString('N'))
  New-Item -ItemType Directory $root | Out-Null
  $start = [Diagnostics.ProcessStartInfo]::new((Join-Path $shell 'powershell.exe'))
  foreach ($word in @('-NoProfile', '-NonInteractive', '-Command', $script)) { $start.ArgumentList.Add($word) }
  $start.Environment.Clear()
  foreach ($name in $kept) {
    $value = [Environment]::GetEnvironmentVariable($name)
    if ($value) { $start.Environment[$name] = $value }
  }
  foreach ($name in $set.Keys) { $start.Environment[$name] = $set[$name] }
  if ($freshProfile) {
    $start.Environment['USERPROFILE'] = $root
    $start.Environment['APPDATA'] = Join-Path $root 'AppData\Roaming'
    $start.Environment['LOCALAPPDATA'] = Join-Path $root 'AppData\Local'
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
$lines += "| image modules, cleared env (control) | $((1..2 | ForEach-Object { Measure-Start 'control' @{} $false }) -join ', ') |"
$folder = Join-Path $env:ProgramFiles 'WindowsPowerShell\Modules'
$aside = Join-Path $env:RUNNER_TEMP 'modules-aside'
New-Item -ItemType Directory -Force $aside | Out-Null
$extra = @(Get-ChildItem -LiteralPath $folder -Directory | Where-Object { $stock -notcontains $_.Name })
foreach ($module in $extra) { Move-Item -LiteralPath $module.FullName -Destination $aside }
$left = @(Get-ChildItem -LiteralPath $folder -Directory | ForEach-Object Name) -join ', '
Write-Host "moved $($extra.Count) image module folders out of Program Files; left: $left"
$stockPath = @{ 'PSModulePath' = (Join-Path $shell 'Modules') }
$lines += "| stock modules, cleared env, same LOCALAPPDATA | $((1..5 | ForEach-Object { Measure-Start 'stock' $stockPath $false }) -join ', ') |"
$lines += "| stock modules, cleared env, fresh profile each start | $((1..5 | ForEach-Object { Measure-Start 'stock-fresh' $stockPath $true }) -join ', ') |"
$lines += "| stock modules, cleared env, no PSModulePath | $((1..5 | ForEach-Object { Measure-Start 'stock-nopath' @{} $false }) -join ', ') |"
$lines += "| image module folders moved aside | $($extra.Count) |"
$lines | Out-File -Append -Encoding utf8 $env:GITHUB_STEP_SUMMARY
$lines | ForEach-Object { Write-Host $_ }
