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
    try {
        $described = [System.Windows.Automation.AutomationProperty]::LookupById(30107)
        $provider = [string]$element.GetCurrentPropertyValue($described)
        "reader_provider=" + ($provider -replace '\s+', ' ')
    } catch {
        "reader_provider_error=" + $_.Exception.GetBaseException().GetType().FullName
    }
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
if ($Mode -ne 'full') { exit 0 }
$unit = [System.Windows.Automation.Text.TextUnit]
$line = $document.Clone()
$line.ExpandToEnclosingUnit($unit::Line)
"reader_line_moved=" + $line.Move($unit::Line, 1)
"reader_line=" + $line.GetText(-1).Replace("`n", "|")
"reader_step=selection"
$selection = $pattern.GetSelection()
"reader_step=selection_text " + $selection.Length
$selected = $selection[0].GetText(-1)
"reader_step=selection_read"
$length = if ($null -eq $selected) { 0 } else { $selected.Length }
"reader_selection=" + $selection.Length + ":" + $length
$rects = $line.GetBoundingRectangles() | ForEach-Object {
    "{0},{1},{2},{3}" -f [int]$_.X, [int]$_.Y, [int]$_.Width, [int]$_.Height
}
"reader_rects=" + ($rects -join ";")
try {
    $document.Select()
    "reader_select=accepted"
} catch {
    "reader_select=refused " + $_.Exception.GetBaseException().GetType().FullName
}
$client = [System.Windows.Automation.AutomationElement].Assembly.Location
$types = [System.Windows.Automation.AutomationEvent].Assembly.Location
Add-Type -ReferencedAssemblies $client, $types -TypeDefinition @"
public static class Seen {
    public static int Count;
    public static void Handler(object sender, System.Windows.Automation.AutomationEventArgs e) {
        System.Threading.Interlocked.Increment(ref Count);
    }
}
"@
$kind = [System.Windows.Automation.AutomationEventHandler]
$handler = [System.Delegate]::CreateDelegate($kind, [Seen].GetMethod('Handler'))
$changed = [System.Windows.Automation.TextPattern]::TextChangedEvent
$scope = [System.Windows.Automation.TreeScope]::Element
$automation = [System.Windows.Automation.Automation]
$automation::AddAutomationEventHandler($changed, $element, $scope, $handler)
"reader_listening"
$deadline = (Get-Date).AddSeconds(15)
while ([Seen]::Count -lt 1 -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 100 }
"reader_text_changed=" + [Seen]::Count
"reader_text_after=" + $document.GetText(-1).Replace("`n", "|")
