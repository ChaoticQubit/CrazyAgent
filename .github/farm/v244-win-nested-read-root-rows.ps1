$ErrorActionPreference = 'Stop'
$red = '4c3e57f01fd6ae76f3f40638b19b3901897568a1'
$tip = 'c5a3835b1e3336a7d1de08586fae765b01839fe4'
$fixture = 'C:\v244\unchangeable-root'
New-Item -ItemType Directory -Force $fixture | Out-Null
icacls (Split-Path -Parent $fixture) /grant '*S-1-15-2-1:(RX)'
if ($LASTEXITCODE -ne 0) { throw "icacls on the fixture's parent failed with $LASTEXITCODE" }
icacls (Split-Path -Parent $fixture)
icacls $fixture /inheritance:r /grant:r '*S-1-5-18:(OI)(CI)(F)' '*S-1-5-32-544:(OI)(CI)(F)' '*S-1-5-32-545:(OI)(CI)(RX)'
if ($LASTEXITCODE -ne 0) { throw "icacls on the fixture failed with $LASTEXITCODE" }
icacls $fixture
[Environment]::SetEnvironmentVariable('CRAZYAGENT_TEST_UNCHANGEABLE_ROOT', $fixture, 'Machine')
$env:CRAZYAGENT_TEST_UNCHANGEABLE_ROOT = $fixture
$user = 'castd'
$alphabet = [char[]]'abcdefghijkmnpqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789'
$password = (-join (1..24 | ForEach-Object { $alphabet | Get-Random })) + 'aZ7!'
net user $user $password /add /y | Out-Null
if ($LASTEXITCODE -ne 0) { throw "net user failed with $LASTEXITCODE" }
$admins = net localgroup Administrators
if ($admins -contains $user) { throw "$user is an administrator" }
icacls $env:GITHUB_WORKSPACE /grant "${user}:(OI)(CI)(RX)" /C /Q | Out-Null
$secure = ConvertTo-SecureString $password -AsPlainText -Force
$logs = Join-Path $env:RUNNER_TEMP 'win-nested-rows'
New-Item -ItemType Directory -Force $logs | Out-Null
$script:verdicts = [ordered]@{}
function Invoke-Row([string]$name, [bool]$asUser, [string]$program, [string[]]$arguments, [bool]$expectPass) {
  $start = [System.Diagnostics.ProcessStartInfo]::new($program)
  foreach ($argument in $arguments) { $start.ArgumentList.Add($argument) }
  if ($asUser) {
    $start.UserName = $user
    $start.Domain = '.'
    $start.Password = $secure
    $start.LoadUserProfile = $true
  }
  $start.UseShellExecute = $false
  $start.WorkingDirectory = $env:GITHUB_WORKSPACE
  $start.RedirectStandardOutput = $true
  $start.RedirectStandardError = $true
  $process = [System.Diagnostics.Process]::Start($start)
  $out = $process.StandardOutput.ReadToEndAsync()
  $err = $process.StandardError.ReadToEndAsync()
  if (-not $process.WaitForExit(1800000)) { $process.Kill($true) }
  $process.WaitForExit()
  Set-Content -LiteralPath (Join-Path $logs "$name.out.txt") -Value $out.Result
  Set-Content -LiteralPath (Join-Path $logs "$name.err.txt") -Value $err.Result
  $passed = $process.ExitCode -eq 0 -and $out.Result -match 'test result: ok\. [1-9]'
  $verdict = if ($passed -eq $expectPass) { 'as expected' } else { 'UNEXPECTED' }
  Write-Host "==== $name exit $($process.ExitCode) passed=$passed expected=$expectPass $verdict"
  Write-Host $out.Result
  Write-Host $err.Result
  $script:verdicts[$name] = "exit $($process.ExitCode) passed=$passed expected=$expectPass $verdict"
}
function Invoke-Plain([string]$name, [string]$program, [string[]]$arguments) {
  $modules = $env:PSModulePath
  Remove-Item Env:PSModulePath
  try { Invoke-Row $name $false $program $arguments $true } finally { $env:PSModulePath = $modules }
}
function Test-Exe([string]$package, [string[]]$selector) {
  $lines = cargo test -p $package @selector --no-run --locked --message-format=json
  if ($LASTEXITCODE -ne 0) { throw "cargo test -p $package $selector --no-run failed with $LASTEXITCODE" }
  $exe = $lines | Where-Object { $_.StartsWith('{') } | ForEach-Object { $_ | ConvertFrom-Json } | Where-Object { $_.reason -eq 'compiler-artifact' -and $_.profile.test -and $_.executable } | Select-Object -Last 1 -ExpandProperty executable
  if (-not $exe) { throw "no test binary for $package $selector" }
  $exe
}
function Checkout([string]$sha) {
  git checkout -q -f --detach $sha
  if ($LASTEXITCODE -ne 0) { throw "checkout $sha failed" }
  Write-Host "######## at $(git log --oneline -1)"
}
function Build-Helper([string]$beside) {
  cargo build -p crazyagent-win-sandbox --bin crazyagent-win-boundary --locked
  if ($LASTEXITCODE -ne 0) { throw "helper build failed with $LASTEXITCODE" }
  Copy-Item -LiteralPath target\debug\crazyagent-win-boundary.exe -Destination (Split-Path -Parent $beside) -Force
}
$reach = 'a_read_root_the_grant_around_it_does_not_reach_is_refused_by_name'
$nested = 'a_read_root_inside_one_every_package_reads_is_named_by_the_helper_not_the_check_before'
$named = 'a_read_root_only_an_administrator_can_open_to_containers_is_refused_by_name'
$granted = 'plan::tests::a_read_root_inside_a_granted_root_is_not_granted_for_itself'
$tree = 'dacl::tests::reads_tree_needs_read_and_run_passed_on_to_files_and_subfolders'
$install = 'sandbox::windows::tests::install_refuses_a_read_root_only_an_administrator_can_open_to_containers'
$node = 'context_mode::windows_tests::a_node_folder_only_an_administrator_can_open_to_containers_is_refused_by_its_own_code'
$one = @('--exact', '--test-threads', '1', '--nocapture')
$ignored = @('--exact', '--ignored', '--test-threads', '1', '--nocapture')

Checkout $red
$boundary = Test-Exe 'crazyagent-win-sandbox' @('--test', 'boundary')
Invoke-Row 'red-reach' $false $boundary (@($reach) + $one) $false

Checkout $tip
$lib = Test-Exe 'crazyagent-win-sandbox' @('--lib')
$boundary = Test-Exe 'crazyagent-win-sandbox' @('--test', 'boundary')
$engine = Test-Exe 'crazyagent' @('--lib')
Build-Helper $engine
Invoke-Row 'tip-reach' $false $boundary (@($reach) + $one) $true
Invoke-Row 'tip-reach-as-castd' $true $boundary (@($reach) + $one) $true
Invoke-Row 'tip-nested-fixture-as-castd' $true $boundary (@($nested) + $ignored) $true
Invoke-Row 'tip-helper-names-path-as-castd' $true $boundary (@($named) + $ignored) $true
Invoke-Row 'tip-install-as-castd' $true $engine (@($install) + $ignored) $true
Invoke-Row 'tip-launch-node-folder-as-castd' $true $engine (@($node) + $ignored) $true
Invoke-Row 'tip-plan-nested' $false $lib (@($granted) + $one) $true
Invoke-Row 'tip-dacl-tree' $false $lib (@($tree) + $one) $true
Invoke-Row 'tip-win-sandbox-lib' $false $lib @('--test-threads', '1') $true
Invoke-Plain 'tip-win-sandbox-boundary' $boundary @('--test-threads', '1')
Invoke-Row 'tip-engine-windows-sandbox' $false $engine @('sandbox::windows::', '--test-threads', '1') $true
Invoke-Row 'tip-engine-context-mode' $false $engine @('context_mode::windows_tests::', '--test-threads', '1') $true
$rows = $script:verdicts.GetEnumerator() | ForEach-Object { "| $($_.Key) | $($_.Value) |" }
@('| row | result |', '| --- | --- |') + $rows | Out-File -Append -Encoding utf8 $env:GITHUB_STEP_SUMMARY
$rows | ForEach-Object { Write-Host $_ }
if ($rows -match 'UNEXPECTED') { Write-Host 'FARM failed=1'; exit 1 }
Write-Host 'FARM failed=0'
exit 0
