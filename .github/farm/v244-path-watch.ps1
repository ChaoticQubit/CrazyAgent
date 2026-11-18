param([string]$Out, [string]$Stop)
$ErrorActionPreference = 'Stop'
$last = $null
$deadline = (Get-Date).AddMinutes(90)
while (-not (Test-Path -LiteralPath $Stop)) {
  if ((Get-Date) -gt $deadline) { exit 3 }
  $k = Get-Item -LiteralPath 'HKCU:\Environment'
  $v = '<none>'
  if ($k.GetValueNames() -contains 'Path') {
    $v = $k.GetValueKind('Path').ToString() + ' ' + [string]$k.GetValue('Path', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
  }
  if ($v -ne $last) {
    Add-Content -LiteralPath $Out -Value $v
    $last = $v
  }
  Start-Sleep -Milliseconds 20
}
