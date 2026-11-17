param([int]$rounds = 3)
$ErrorActionPreference = 'Stop'
$shell = Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0'
$machineModules = [Environment]::GetEnvironmentVariable('PSModulePath', 'Machine')
$kept = @('PATH', 'PATHEXT', 'SystemRoot', 'SystemDrive', 'windir', 'ComSpec', 'USERPROFILE', 'USERNAME', 'APPDATA', 'LOCALAPPDATA', 'TEMP', 'TMP')
$parent = @{}
foreach ($entry in [Environment]::GetEnvironmentVariables().GetEnumerator()) { $parent[$entry.Key] = $entry.Value }
$parent['PSModulePath'] = $machineModules
function Get-Env([string]$mode, [string]$root) {
  $set = @{}
  if ($mode -eq 'full') {
    foreach ($name in $parent.Keys) { $set[$name] = $parent[$name] }
    return $set
  }
  foreach ($name in $kept) { if ($parent[$name]) { $set[$name] = $parent[$name] } }
  if ($mode -eq 'version-path') {
    $set['USERPROFILE'] = $root
    $set['PATH'] = "$shell;$env:SystemRoot\System32;$env:SystemRoot"
    return $set
  }
  $extra = switch ($mode) {
    'check-path' { @() }
    'plus-all' { @('PSModulePath', 'ProgramFiles', 'ProgramData', 'ALLUSERSPROFILE', 'PROCESSOR_ARCHITECTURE') }
    'plus-PSModulePath' { @('PSModulePath') }
    'plus-ProgramFiles' { @('ProgramFiles') }
    'plus-ProgramData' { @('ProgramData', 'ALLUSERSPROFILE') }
    'plus-PROCESSOR_ARCHITECTURE' { @('PROCESSOR_ARCHITECTURE') }
  }
  foreach ($name in $extra) { if ($parent[$name]) { $set[$name] = $parent[$name] } }
  return $set
}
$script = "Set-Content -LiteralPath started.txt -Value ((Get-Date).ToString('o')); `$p = Start-Process -FilePath ping.exe -ArgumentList '-n','2','127.0.0.1' -NoNewWindow -PassThru; Set-Content -LiteralPath grandchild.pid -Value `$p.Id"
$modes = @('full', 'version-path', 'check-path', 'plus-all', 'plus-PSModulePath', 'plus-ProgramFiles', 'plus-ProgramData', 'plus-PROCESSOR_ARCHITECTURE')
$results = [ordered]@{}
foreach ($mode in $modes) { $results[$mode] = [System.Collections.Generic.List[string]]::new() }
for ($round = 1; $round -le $rounds; $round++) {
  foreach ($mode in $modes) {
    $root = Join-Path ([IO.Path]::GetTempPath()) ('probe-' + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory $root | Out-Null
    $start = [Diagnostics.ProcessStartInfo]::new((Join-Path $shell 'powershell.exe'))
    foreach ($word in @('-NoProfile', '-NonInteractive', '-Command', $script)) { $start.ArgumentList.Add($word) }
    $start.Environment.Clear()
    $values = Get-Env $mode $root
    foreach ($name in $values.Keys) { $start.Environment[$name] = $values[$name] }
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.WorkingDirectory = $root
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $clock = [Diagnostics.Stopwatch]::StartNew()
    $child = [Diagnostics.Process]::Start($start)
    $out = $child.StandardOutput.ReadToEndAsync()
    $err = $child.StandardError.ReadToEndAsync()
    $started = 'none'
    while ($clock.Elapsed.TotalSeconds -lt 90) {
      if (Test-Path -LiteralPath (Join-Path $root 'started.txt')) { $started = [math]::Round($clock.Elapsed.TotalSeconds, 2); break }
      if ($child.HasExited) { break }
      Start-Sleep -Milliseconds 50
    }
    if (-not $child.WaitForExit(30000)) { $child.Kill($true) }
    $child.WaitForExit()
    $results[$mode].Add("$started")
    Write-Host "round $round mode $mode env $($values.Count) names: first statement at $started s, exit $($child.ExitCode), wrote $((Get-ChildItem -LiteralPath $root -Force | ForEach-Object Name) -join ',')"
    if ($err.Result.Trim()) { Write-Host "  stderr: $($err.Result.Trim())" }
    if ($round -eq 1 -and $mode -eq 'version-path') {
      Get-ChildItem -LiteralPath $root -Recurse -Force | Select-Object -First 40 | ForEach-Object { Write-Host "  tree: $($_.FullName.Substring($root.Length)) $($_.Length)" }
    }
  }
}
$lines = @('| mode | first statement at (s), each start |', '| --- | --- |')
foreach ($mode in $modes) { $lines += "| $mode | $($results[$mode] -join ', ') |" }
$lines | Out-File -Append -Encoding utf8 $env:GITHUB_STEP_SUMMARY
$lines | ForEach-Object { Write-Host $_ }
