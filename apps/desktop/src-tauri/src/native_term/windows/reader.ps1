param([Int64]$Handle, [string]$Mode)
$ErrorActionPreference = 'Stop'
"reader_pid=$PID"
"reader_language=" + $ExecutionContext.SessionState.LanguageMode
$wpf = 'Version=4.0.0.0, Culture=neutral, PublicKeyToken=31bf3856ad364e35'
try {
    $null = [System.Reflection.Assembly]::Load("UIAutomationClient, $wpf")
    $null = [System.Reflection.Assembly]::Load("UIAutomationTypes, $wpf")
    "reader_stage=lookup"
    $element = [System.Windows.Automation.AutomationElement]::FromHandle([IntPtr]$Handle)
    $current = $element.Current
    "reader_hwnd=" + $current.NativeWindowHandle
    "reader_process=" + $current.ProcessId
    "reader_control=" + $current.ControlType.Id
    "reader_name=" + $current.Name
    $pattern = $null
    $textPattern = [System.Windows.Automation.TextPattern]::Pattern
    $has = $element.TryGetCurrentPattern($textPattern, [ref]$pattern)
    "reader_text_pattern=$has"
    if (-not $has) { exit 0 }
    $document = $pattern.DocumentRange
    "reader_text=" + $document.GetText(-1).Replace("`n", "|")
} catch {
    $base = $_.Exception.GetBaseException()
    "reader_error=" + $base.GetType().FullName + " " + ('0x{0:X8}' -f $base.HResult)
    "reader_error_message=" + ($base.Message -replace '\s+', ' ')
    exit 0
}
function Measured($read) {
    if ($null -eq $read) { 'null' } else { $read.Length }
}
if ($Mode -eq 'probe_visible') {
    $visible = $pattern.GetVisibleRanges()
    "reader_step=visible_array " + $visible.Length
    $first = $visible[0]
    "reader_step=visible_indexed"
    "reader_step=visible_read " + (Measured $first.GetText(-1))
    exit 0
}
if ($Mode -eq 'probe_empty') {
    $endpoint = [System.Windows.Automation.Text.TextPatternRangeEndpoint]
    $empty = $document.Clone()
    $empty.MoveEndpointByRange($endpoint::End, $empty, $endpoint::Start)
    "reader_step=empty_collapsed"
    "reader_step=empty_read " + (Measured $empty.GetText(-1))
    exit 0
}
if ($Mode -eq 'probe_selection') {
    $selection = $pattern.GetSelection()
    "reader_step=selection_array " + $selection.Length
    $first = $selection[0]
    "reader_step=selection_indexed"
    $null = $first.Clone()
    "reader_step=selection_cloned"
    "reader_step=selection_read " + (Measured $first.GetText(-1))
    exit 0
}
