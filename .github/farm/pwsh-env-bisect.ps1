$ErrorActionPreference = 'Stop'
$shell = Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0'
$kept = @('PATH', 'PATHEXT', 'SystemRoot', 'SystemDrive', 'windir', 'ComSpec', 'USERPROFILE', 'USERNAME', 'APPDATA', 'LOCALAPPDATA', 'TEMP', 'TMP')
$parent = @{}
foreach ($entry in [Environment]::GetEnvironmentVariables().GetEnumerator()) { $parent[[string]$entry.Key] = [string]$entry.Value }
$parent['PSModulePath'] = [Environment]::GetEnvironmentVariable('PSModulePath', 'Machine')
$extra = @($parent.Keys | Where-Object { $name = $_; -not ($kept | Where-Object { $_ -ieq $name }) } | Sort-Object)
$script = "Set-Content -LiteralPath started.txt -Value 1"
$fastBelow = 5
$script:tests = 0
function Measure-Start([string[]]$names) {
  $script:tests++
  $root = Join-Path ([IO.Path]::GetTempPath()) ('bisect-' + [guid]::NewGuid().ToString('N'))
  New-Item -ItemType Directory $root | Out-Null
  $start = [Diagnostics.ProcessStartInfo]::new((Join-Path $shell 'powershell.exe'))
  foreach ($word in @('-NoProfile', '-NonInteractive', '-Command', $script)) { $start.ArgumentList.Add($word) }
  $start.Environment.Clear()
  foreach ($name in ($kept + $names)) { if ($parent.ContainsKey($name)) { $start.Environment[$name] = $parent[$name] } }
  $start.UseShellExecute = $false
  $start.CreateNoWindow = $true
  $start.WorkingDirectory = $root
  $start.RedirectStandardOutput = $true
  $start.RedirectStandardError = $true
  $clock = [Diagnostics.Stopwatch]::StartNew()
  $child = [Diagnostics.Process]::Start($start)
  $null = $child.StandardOutput.ReadToEndAsync()
  $null = $child.StandardError.ReadToEndAsync()
  $seconds = 60
  while ($clock.Elapsed.TotalSeconds -lt 60) {
    if (Test-Path -LiteralPath (Join-Path $root 'started.txt')) { $seconds = [math]::Round($clock.Elapsed.TotalSeconds, 2); break }
    if ($child.HasExited) { break }
    Start-Sleep -Milliseconds 50
  }
  if (-not $child.WaitForExit(20000)) { $child.Kill($true) }
  Write-Host "test $($script:tests): $($names.Count) added names -> $seconds s"
  return $seconds
}
function Test-Fast([string[]]$names) { return (Measure-Start $names) -lt $fastBelow }
function Get-Minimal([string[]]$fixed, [string[]]$candidates) {
  if ($candidates.Count -le 1) { return ,$candidates }
  $half = [math]::Floor($candidates.Count / 2)
  $a = $candidates[0..($half - 1)]
  $b = $candidates[$half..($candidates.Count - 1)]
  if (Test-Fast ($fixed + $a)) { return ,(Get-Minimal $fixed $a) }
  if (Test-Fast ($fixed + $b)) { return ,(Get-Minimal $fixed $b) }
  $keptA = Get-Minimal ($fixed + $b) $a
  $keptB = Get-Minimal ($fixed + $keptA) $b
  return ,($keptA + $keptB)
}
Write-Host "kept names: $($kept.Count), missing names: $($extra.Count)"
$warm = Measure-Start $extra
$base = Measure-Start @()
$all = Measure-Start $extra
if ($all -ge $fastBelow -or $base -lt $fastBelow) {
  Write-Host "no bisect: base $base s, all names $all s"
  "| bisect | base $base s, all $all s, no bisect |" | Out-File -Append -Encoding utf8 $env:GITHUB_STEP_SUMMARY
  exit 0
}
$minimal = Get-Minimal @() $extra
Write-Host "minimal set: $($minimal -join ', ')"
$lines = @('| check | first statement at (s), three starts |', '| --- | --- |')
$lines += "| kept + minimal ($($minimal -join ', ')) | $((1..3 | ForEach-Object { Measure-Start $minimal }) -join ', ') |"
foreach ($name in $minimal) {
  $rest = @($minimal | Where-Object { $_ -ne $name })
  $lines += "| kept + minimal - $name | $((1..3 | ForEach-Object { Measure-Start $rest }) -join ', ') |"
}
$lines += "| kept only | $((1..2 | ForEach-Object { Measure-Start @() }) -join ', ') |"
$lines | Out-File -Append -Encoding utf8 $env:GITHUB_STEP_SUMMARY
$lines | ForEach-Object { Write-Host $_ }
