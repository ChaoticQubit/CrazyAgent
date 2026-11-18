use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The out-of-process reader: Windows PowerShell's UI Automation client asks the view's
/// window for its control type, name and text pattern, reads the whole document range and
/// tries to select it. Every line it prints is "key=value".
const READER: &str = r##"param([Int64]$Handle)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
"reader_pid=$PID"
$element = [System.Windows.Automation.AutomationElement]::FromHandle([IntPtr]$Handle)
"reader_control_type=" + $element.Current.ControlType.ProgrammaticName
"reader_name=" + $element.Current.Name
$pattern = $null
$has = $element.TryGetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern, [ref]$pattern)
"reader_text_pattern=$has"
if ($has) {
    $text = $pattern.DocumentRange.GetText(-1)
    "reader_text_chars=" + $text.Length
    "reader_text_tail=" + $text.Substring([Math]::Max(0, $text.Length - 24)).Replace("`n", "|")
    try {
        $pattern.DocumentRange.Select()
        "reader_select=accepted"
    } catch {
        "reader_select=refused " + $_.Exception.GetBaseException().GetType().FullName
    }
}
"##;

/// How long the reader may run: Windows PowerShell's start and its assembly loads take a
/// few seconds on a hosted runner, and a hung reader must not hold the run.
const READER_WAIT: Duration = Duration::from_secs(45);

/// Lines of the reader's error output kept in the log.
const READER_ERROR_LINES: usize = 6;

/// Runs the reader in its own process against the view's window, the way a screen reader
/// reaches it. Called off the window's thread, which must stay free to answer the
/// reader's WM_GETOBJECT. Input: the view's window. Output: the reader's lines with its
/// exit and time, or why it could not start.
pub fn read(hwnd: isize) -> Result<Vec<String>, String> {
    assert!(hwnd != 0, "the view exists before it is read");
    let dir = std::env::temp_dir().join(format!("spike-uia-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let script = dir.join("reader.ps1");
    std::fs::write(&script, READER).map_err(|e| e.to_string())?;
    let (out, err) = (dir.join("out.txt"), dir.join("err.txt"));
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    let shell = PathBuf::from(root).join("System32\\WindowsPowerShell\\v1.0\\powershell.exe");
    let started = Instant::now();
    let mut child = Command::new(shell)
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(&script)
        .arg(hwnd.to_string())
        .env_remove("PSModulePath")
        .stdin(Stdio::null())
        .stdout(std::fs::File::create(&out).map_err(|e| e.to_string())?)
        .stderr(std::fs::File::create(&err).map_err(|e| e.to_string())?)
        .spawn()
        .map_err(|e| e.to_string())?;
    let exit = loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            break format!("{:?}", status.code());
        }
        if started.elapsed() > READER_WAIT {
            let _ = child.kill();
            let _ = child.wait();
            break "killed".to_string();
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let text = std::fs::read_to_string(&out).unwrap_or_default();
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let errors = std::fs::read_to_string(&err).unwrap_or_default();
    lines.extend(
        errors
            .lines()
            .take(READER_ERROR_LINES)
            .map(|line| format!("reader_stderr={line}")),
    );
    lines.push(format!(
        "reader_exit={exit} ms={}",
        started.elapsed().as_millis()
    ));
    assert!(!lines.is_empty());
    Ok(lines)
}

#[cfg(feature = "uia")]
pub use provider::answer;

/// A minimal UI Automation provider on the view's window: a read-only Document whose text
/// is the screen the view draws. Mutating calls are refused, as the macOS view refuses
/// them. Every call logs whether it ran on the window's thread.
#[cfg(feature = "uia")]
mod provider {
    use std::cell::RefCell;
    use windows::Win32::Foundation::{E_NOTIMPL, HWND, LPARAM, WPARAM};
    use windows::Win32::System::Com::SAFEARRAY;
    use windows::Win32::System::Variant::VARIANT;
    use windows::Win32::UI::Accessibility::{
        IRawElementProviderSimple, IRawElementProviderSimple_Impl, ITextProvider,
        ITextProvider_Impl, ITextRangeProvider, ITextRangeProvider_Impl, ProviderOptions,
        ProviderOptions_ServerSideProvider, SupportedTextSelection, SupportedTextSelection_None,
        TextPatternRangeEndpoint, TextUnit, UIA_ControlTypePropertyId, UIA_DocumentControlTypeId,
        UIA_E_INVALIDOPERATION, UIA_IsKeyboardFocusablePropertyId, UIA_NamePropertyId,
        UIA_PATTERN_ID, UIA_PROPERTY_ID, UIA_TEXTATTRIBUTE_ID, UIA_TextPatternId,
        UiaHostProviderFromHwnd, UiaPoint, UiaReturnRawElementProvider, UiaRootObjectId,
    };
    use windows_core::{
        BOOL, BSTR, Error, HRESULT, IUnknown, IUnknownImpl, Interface, Ref, Result, implement,
    };

    thread_local! {
        static ROOT: RefCell<Option<IRawElementProviderSimple>> = const { RefCell::new(None) };
    }

    /// Logs one provider call and whether it ran on the window's thread.
    /// Input: the method name.
    fn called(method: &str) {
        let thread = unsafe { windows_sys::Win32::System::Threading::GetCurrentThreadId() };
        let ui = thread == crate::probe::ui_thread();
        crate::probe::log("uia_call", &format!("{method} ui_thread={ui}"));
    }

    /// The error a refused mutation returns, the one UI Automation names for it.
    fn refused() -> Error {
        Error::from(HRESULT(UIA_E_INVALIDOPERATION as i32))
    }

    /// Answers WM_GETOBJECT for the root object with the view's provider, made once on the
    /// window's thread. Inputs: the message's window and parameters. Output: the result to
    /// return, or None for another object id, which goes to the default procedure.
    pub fn answer(hwnd: isize, wparam: usize, lparam: isize) -> Option<isize> {
        if lparam as i32 != UiaRootObjectId {
            return None;
        }
        let root = ROOT.with(|root| {
            root.borrow_mut()
                .get_or_insert_with(|| Terminal { hwnd }.into())
                .clone()
        });
        called("WM_GETOBJECT");
        let window = HWND(hwnd as *mut core::ffi::c_void);
        let result =
            unsafe { UiaReturnRawElementProvider(window, WPARAM(wparam), LPARAM(lparam), &root) };
        Some(result.0)
    }

    /// The view's element: a keyboard-focusable Document named "Terminal".
    #[implement(IRawElementProviderSimple, ITextProvider)]
    struct Terminal {
        hwnd: isize,
    }

    /// The whole screen as one range; it never moves, since the spike reads only.
    #[implement(ITextRangeProvider)]
    struct TextRange {
        hwnd: isize,
    }

    impl IRawElementProviderSimple_Impl for Terminal_Impl {
        fn ProviderOptions(&self) -> Result<ProviderOptions> {
            called("ProviderOptions");
            Ok(ProviderOptions_ServerSideProvider)
        }

        fn GetPatternProvider(&self, patternid: UIA_PATTERN_ID) -> Result<IUnknown> {
            called("GetPatternProvider");
            if patternid != UIA_TextPatternId {
                return Err(Error::empty());
            }
            let text: ITextProvider = self.to_interface();
            text.cast()
        }

        fn GetPropertyValue(&self, propertyid: UIA_PROPERTY_ID) -> Result<VARIANT> {
            if propertyid == UIA_ControlTypePropertyId {
                return Ok(VARIANT::from(UIA_DocumentControlTypeId.0));
            }
            if propertyid == UIA_NamePropertyId {
                return Ok(VARIANT::from("Terminal"));
            }
            if propertyid == UIA_IsKeyboardFocusablePropertyId {
                return Ok(VARIANT::from(true));
            }
            Ok(VARIANT::default())
        }

        fn HostRawElementProvider(&self) -> Result<IRawElementProviderSimple> {
            unsafe { UiaHostProviderFromHwnd(HWND(self.hwnd as *mut core::ffi::c_void)) }
        }
    }

    impl ITextProvider_Impl for Terminal_Impl {
        fn GetSelection(&self) -> Result<*mut SAFEARRAY> {
            Err(E_NOTIMPL.into())
        }

        fn GetVisibleRanges(&self) -> Result<*mut SAFEARRAY> {
            Err(E_NOTIMPL.into())
        }

        fn RangeFromChild(
            &self,
            _childelement: Ref<'_, IRawElementProviderSimple>,
        ) -> Result<ITextRangeProvider> {
            Err(E_NOTIMPL.into())
        }

        fn RangeFromPoint(&self, _point: &UiaPoint) -> Result<ITextRangeProvider> {
            Err(E_NOTIMPL.into())
        }

        fn DocumentRange(&self) -> Result<ITextRangeProvider> {
            called("DocumentRange");
            Ok(TextRange { hwnd: self.hwnd }.into())
        }

        fn SupportedTextSelection(&self) -> Result<SupportedTextSelection> {
            Ok(SupportedTextSelection_None)
        }
    }

    impl ITextRangeProvider_Impl for TextRange_Impl {
        fn Clone(&self) -> Result<ITextRangeProvider> {
            Ok(TextRange { hwnd: self.hwnd }.into())
        }

        fn Compare(&self, _range: Ref<'_, ITextRangeProvider>) -> Result<BOOL> {
            Ok(true.into())
        }

        fn CompareEndpoints(
            &self,
            _endpoint: TextPatternRangeEndpoint,
            _targetrange: Ref<'_, ITextRangeProvider>,
            _targetendpoint: TextPatternRangeEndpoint,
        ) -> Result<i32> {
            Ok(0)
        }

        fn ExpandToEnclosingUnit(&self, _unit: TextUnit) -> Result<()> {
            Ok(())
        }

        fn FindAttribute(
            &self,
            _attributeid: UIA_TEXTATTRIBUTE_ID,
            _val: &VARIANT,
            _backward: BOOL,
        ) -> Result<ITextRangeProvider> {
            Err(Error::empty())
        }

        fn FindText(
            &self,
            _text: &BSTR,
            _backward: BOOL,
            _ignorecase: BOOL,
        ) -> Result<ITextRangeProvider> {
            Err(Error::empty())
        }

        fn GetAttributeValue(&self, _attributeid: UIA_TEXTATTRIBUTE_ID) -> Result<VARIANT> {
            Err(E_NOTIMPL.into())
        }

        fn GetBoundingRectangles(&self) -> Result<*mut SAFEARRAY> {
            Err(E_NOTIMPL.into())
        }

        fn GetEnclosingElement(&self) -> Result<IRawElementProviderSimple> {
            Ok(Terminal { hwnd: self.hwnd }.into())
        }

        fn GetText(&self, maxlength: i32) -> Result<BSTR> {
            called("GetText");
            let text = crate::gpu::screen_text(&crate::probe::typed());
            let units: Vec<u16> = text.encode_utf16().collect();
            let keep = usize::try_from(maxlength).map_or(units.len(), |max| max.min(units.len()));
            Ok(BSTR::from_wide(&units[..keep]))
        }

        fn Move(&self, _unit: TextUnit, _count: i32) -> Result<i32> {
            Ok(0)
        }

        fn MoveEndpointByUnit(
            &self,
            _endpoint: TextPatternRangeEndpoint,
            _unit: TextUnit,
            _count: i32,
        ) -> Result<i32> {
            Ok(0)
        }

        fn MoveEndpointByRange(
            &self,
            _endpoint: TextPatternRangeEndpoint,
            _targetrange: Ref<'_, ITextRangeProvider>,
            _targetendpoint: TextPatternRangeEndpoint,
        ) -> Result<()> {
            Ok(())
        }

        fn Select(&self) -> Result<()> {
            called("Select");
            Err(refused())
        }

        fn AddToSelection(&self) -> Result<()> {
            Err(refused())
        }

        fn RemoveFromSelection(&self) -> Result<()> {
            Err(refused())
        }

        fn ScrollIntoView(&self, _aligntotop: BOOL) -> Result<()> {
            Ok(())
        }

        fn GetChildren(&self) -> Result<*mut SAFEARRAY> {
            Err(E_NOTIMPL.into())
        }
    }
}
