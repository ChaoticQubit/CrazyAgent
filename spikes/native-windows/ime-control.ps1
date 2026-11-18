Add-Type -AssemblyName System.Windows.Forms
$form = New-Object System.Windows.Forms.Form
$form.Text = "spike-ime-control"
$form.TopMost = $true
$box = New-Object System.Windows.Forms.TextBox
$box.Dock = [System.Windows.Forms.DockStyle]::Fill
$form.Controls.Add($box)
$form.Show()
$form.Activate()
[void]$box.Focus()
[System.Windows.Forms.Application]::DoEvents()
"control_ime start focused=$($box.Focused) at=$([DateTime]::UtcNow.ToString('HH:mm:ss.fff'))"
foreach ($tag in @("ko-KR", "ja-JP", "ko-KR")) {
    $culture = [System.Globalization.CultureInfo]::GetCultureInfo($tag)
    $language = [System.Windows.Forms.InputLanguage]::FromCulture($culture)
    if ($null -eq $language) {
        "control_ime $tag missing"
        continue
    }
    $watch = [System.Diagnostics.Stopwatch]::StartNew()
    [System.Windows.Forms.InputLanguage]::CurrentInputLanguage = $language
    $switched = $watch.ElapsedMilliseconds
    $box.ImeMode = [System.Windows.Forms.ImeMode]::On
    [System.Windows.Forms.Application]::DoEvents()
    $watch.Stop()
    $now = [System.Windows.Forms.InputLanguage]::CurrentInputLanguage.Culture.Name
    "control_ime $tag switch_ms=$switched open_ms=$($watch.ElapsedMilliseconds) now=$now ime=$($box.ImeMode) at=$([DateTime]::UtcNow.ToString('HH:mm:ss.fff'))"
}
$culture = [System.Globalization.CultureInfo]::GetCultureInfo("ja-JP")
$language = [System.Windows.Forms.InputLanguage]::FromCulture($culture)
if ($null -ne $language) {
    [System.Windows.Forms.InputLanguage]::CurrentInputLanguage = $language
    $box.ImeMode = [System.Windows.Forms.ImeMode]::Hiragana
    $box.Text = ""
    $form.Activate()
    [void]$box.Focus()
    [System.Windows.Forms.Application]::DoEvents()
    "control_ime convert start at=$([DateTime]::UtcNow.ToString('HH:mm:ss.fff'))"
    $watch = [System.Diagnostics.Stopwatch]::StartNew()
    [System.Windows.Forms.SendKeys]::SendWait("ka")
    [System.Windows.Forms.Application]::DoEvents()
    $composed = $watch.ElapsedMilliseconds
    [System.Windows.Forms.SendKeys]::SendWait(" ")
    [System.Windows.Forms.Application]::DoEvents()
    $converted = $watch.ElapsedMilliseconds
    [System.Windows.Forms.SendKeys]::SendWait(" ")
    [System.Windows.Forms.Application]::DoEvents()
    $listed = $watch.ElapsedMilliseconds
    [System.Windows.Forms.SendKeys]::SendWait("{ENTER}")
    [System.Windows.Forms.Application]::DoEvents()
    $watch.Stop()
    $units = ($box.Text.ToCharArray() | ForEach-Object { "{0:X4}" -f [int]$_ }) -join " "
    "control_ime convert composed_ms=$composed converted_ms=$converted listed_ms=$listed committed_ms=$($watch.ElapsedMilliseconds) text=$units at=$([DateTime]::UtcNow.ToString('HH:mm:ss.fff'))"
}
$form.Close()
"control_ime done"
