param([string]$label)
$ErrorActionPreference = 'Stop'
$root = Join-Path ([IO.Path]::GetTempPath()) ('probe-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory $root | Out-Null
$shell = Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0'
$script = "Set-Content -LiteralPath started.txt -Value ((Get-Date).ToString('o') + ' ' + `$PWD.Path); `$p = Start-Process -FilePath ping.exe -ArgumentList '-n','3','127.0.0.1' -NoNewWindow -PassThru; Set-Content -LiteralPath grandchild.pid -Value `$p.Id"
$start = [Diagnostics.ProcessStartInfo]::new((Join-Path $shell 'powershell.exe'))
foreach ($word in @('-NoProfile', '-NonInteractive', '-Command', $script)) { $start.ArgumentList.Add($word) }
$kept = @('PATHEXT', 'SystemRoot', 'SystemDrive', 'windir', 'ComSpec', 'USERNAME', 'APPDATA', 'LOCALAPPDATA', 'TEMP', 'TMP')
$values = @{}
foreach ($name in $kept) { $values[$name] = [Environment]::GetEnvironmentVariable($name) }
$start.Environment.Clear()
foreach ($name in $kept) { if ($values[$name]) { $start.Environment[$name] = $values[$name] } }
$start.Environment['USERPROFILE'] = $root
$start.Environment['PATH'] = "$shell;$env:SystemRoot\System32;$env:SystemRoot"
$start.UseShellExecute = $false
$start.CreateNoWindow = $true
$start.WorkingDirectory = $root
$start.RedirectStandardOutput = $true
$start.RedirectStandardError = $true
$clock = [Diagnostics.Stopwatch]::StartNew()
$child = [Diagnostics.Process]::Start($start)
$out = $child.StandardOutput.ReadToEndAsync()
$err = $child.StandardError.ReadToEndAsync()
$started = $null
$written = $null
while ($clock.Elapsed.TotalSeconds -lt 120) {
  if (-not $started -and (Test-Path -LiteralPath (Join-Path $root 'started.txt'))) { $started = [math]::Round($clock.Elapsed.TotalSeconds, 2) }
  if (-not $written -and (Test-Path -LiteralPath (Join-Path $root 'grandchild.pid'))) { $written = [math]::Round($clock.Elapsed.TotalSeconds, 2) }
  if ($written -or $child.HasExited) { break }
  Start-Sleep -Milliseconds 100
}
$exited = $child.HasExited
if (-not $exited) { $child.Kill($true) }
$child.WaitForExit()
"probe $label user $($env:USERNAME): started.txt at $started s, grandchild.pid at $written s, exited by itself $exited, code $($child.ExitCode)"
Get-ChildItem -LiteralPath $root | ForEach-Object { "  $($_.Name): $((Get-Content -LiteralPath $_.FullName -Raw).Trim())" }
"  stdout: $($out.Result)"
"  stderr: $($err.Result)"
