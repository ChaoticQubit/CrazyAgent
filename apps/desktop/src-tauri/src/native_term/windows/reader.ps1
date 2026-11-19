param([Int64]$Handle, [string]$Mode)
$ErrorActionPreference = 'Stop'
"reader_pid=$PID"
try {
    Add-Type -AssemblyName UIAutomationClient
    Add-Type -AssemblyName UIAutomationTypes
    $element = [System.Windows.Automation.AutomationElement]::FromHandle([IntPtr]$Handle)
    "reader_name=" + $element.Current.Name
    $pattern = $null
    $has = $element.TryGetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern, [ref]$pattern)
    "reader_text_pattern=$has"
    if (-not $has) { exit 0 }
    $document = $pattern.DocumentRange
    "reader_text=" + $document.GetText(-1).Replace("`n", "|")
} catch {
    "reader_error=" + $_.Exception.GetBaseException().GetType().FullName
    exit 0
}
if ($Mode -ne 'full') { exit 0 }
$unit = [System.Windows.Automation.Text.TextUnit]
$line = $document.Clone()
$line.ExpandToEnclosingUnit($unit::Line)
"reader_line_moved=" + $line.Move($unit::Line, 1)
"reader_line=" + $line.GetText(-1).Replace("`n", "|")
$selection = $pattern.GetSelection()
"reader_selection=" + $selection.Length + ":" + $selection[0].GetText(-1).Length
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
[System.Windows.Automation.Automation]::AddAutomationEventHandler($changed, $element, $scope, $handler)
"reader_listening"
$deadline = (Get-Date).AddSeconds(15)
while ([Seen]::Count -lt 1 -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 100 }
"reader_text_changed=" + [Seen]::Count
"reader_text_after=" + $document.GetText(-1).Replace("`n", "|")
