use std::sync::{Arc, Mutex, MutexGuard};

use windows::Win32::Foundation::{E_INVALIDARG, HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::SAFEARRAY;
use windows::Win32::System::Ole::{SafeArrayCreateVector, SafeArrayDestroy, SafeArrayPutElement};
use windows::Win32::System::Variant::{VARIANT, VT_R8, VT_UNKNOWN};
use windows::Win32::UI::Accessibility::{
    IRawElementProviderSimple, IRawElementProviderSimple_Impl, ITextProvider, ITextProvider_Impl,
    ITextRangeProvider, ITextRangeProvider_Impl, ProviderOptions,
    ProviderOptions_ServerSideProvider, SupportedTextSelection, SupportedTextSelection_Single,
    TextPatternRangeEndpoint, TextPatternRangeEndpoint_End, TextPatternRangeEndpoint_Start,
    TextUnit, TextUnit_Character, TextUnit_Document, TextUnit_Format, TextUnit_Line, TextUnit_Page,
    TextUnit_Paragraph, TextUnit_Word, UIA_ControlTypePropertyId, UIA_DocumentControlTypeId,
    UIA_E_ELEMENTNOTAVAILABLE, UIA_E_INVALIDOPERATION, UIA_EVENT_ID,
    UIA_HasKeyboardFocusPropertyId, UIA_IsKeyboardFocusablePropertyId, UIA_NamePropertyId,
    UIA_PATTERN_ID, UIA_PROPERTY_ID, UIA_TEXTATTRIBUTE_ID, UIA_Text_TextChangedEventId,
    UIA_Text_TextSelectionChangedEventId, UIA_TextPatternId, UiaClientsAreListening,
    UiaDisconnectProvider, UiaGetReservedNotSupportedValue, UiaHostProviderFromHwnd, UiaPoint,
    UiaRaiseAutomationEvent, UiaReturnRawElementProvider, UiaRootObjectId,
};
use windows_core::{
    BOOL, BSTR, Error, HRESULT, IUnknown, IUnknownImpl, Interface, Ref, Result, implement,
};

use super::super::a11y::range::{Unit, cell_near};
use super::super::a11y::{self, Geometry, Grid, Marks, Text};
use super::draw::BOARD;

/// The name every view's element carries until the host gives it the pane's title (H5).
const NAME: &str = "Terminal";

/// What changed in one post: the text, so UI Automation's text-changed event is owed, and
/// the cursor or selection, so its selection-changed event is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Changed {
    pub text: bool,
    pub selection: bool,
}

/// What one view shows UI Automation, shared between its window thread, which posts it, and
/// the threads UI Automation calls the provider on. One lock; no call holds it while it
/// holds another.
pub struct Access {
    id: u64,
    state: Mutex<State>,
}

struct State {
    grid: Arc<Grid>,
    text: Option<Arc<Text>>,
    marks: Marks,
    text_key: u64,
    key: u32,
    focused: bool,
    gone: bool,
}

/// The error every call on a removed view's provider or range answers.
fn not_available() -> Error {
    Error::from(HRESULT(UIA_E_ELEMENTNOTAVAILABLE as i32))
}

/// The error UI Automation names for a refused change, the answer to every selection call.
fn refused() -> Error {
    Error::from(HRESULT(UIA_E_INVALIDOPERATION as i32))
}

impl Access {
    /// The access of a new view, with an empty screen. Inputs: the view id and its scale key.
    /// Output: the shared access.
    pub fn new(id: u64, key: u32) -> Arc<Access> {
        assert!((100..=400).contains(&key));
        let access = Access {
            id,
            state: Mutex::new(State {
                grid: Arc::new(Grid::default()),
                text: None,
                marks: Marks::default(),
                text_key: 0,
                key,
                focused: false,
                gone: false,
            }),
        };
        Arc::new(access)
    }

    /// Takes the lock; a reader that panicked under it leaves state a later read rebuilds,
    /// so a poisoned lock is taken as it is.
    fn held(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Posts the screen a frame drew, keeping the built text while nothing it shows changed.
    /// Inputs: the visible grid (`a11y::visible`) and `a11y::text_key` of the same terminal.
    /// Output: what changed.
    pub fn post(&self, grid: Grid, text_key: u64) -> Changed {
        let marks = Marks {
            cursor: grid.cursor,
            selection: grid.selection,
        };
        let mut state = self.held();
        let changed = Changed {
            text: state.text_key != text_key,
            selection: state.marks != marks,
        };
        if changed.text || changed.selection {
            state.grid = Arc::new(grid);
            state.text = None;
            state.marks = marks;
            state.text_key = text_key;
        }
        assert!(state.text.is_none() || !(changed.text || changed.selection));
        changed
    }

    /// Sets the scale key the view now draws at. Input: the key.
    pub fn rescale(&self, key: u32) {
        assert!((100..=400).contains(&key));
        self.held().key = key;
    }

    /// Sets whether the view holds keyboard focus. Input: the focus.
    pub fn focus(&self, focused: bool) {
        self.held().focused = focused;
    }

    /// Ends the view's access: every later call answers `UIA_E_ELEMENTNOTAVAILABLE`.
    pub fn end(&self) {
        let mut state = self.held();
        state.gone = true;
        state.text = None;
        assert!(state.gone);
    }

    /// The text of the newest screen. Output: the text, or `UIA_E_ELEMENTNOTAVAILABLE`
    /// once the view is gone.
    fn text(&self) -> Result<Arc<Text>> {
        Ok(self.read()?.0)
    }

    /// The text of the newest screen with the rows and columns of the grid it was built
    /// from, built outside the lock on the first read after a post. Output: the text and
    /// its grid size, or `UIA_E_ELEMENTNOTAVAILABLE` once the view is gone.
    fn read(&self) -> Result<(Arc<Text>, (usize, usize))> {
        let (grid, key) = {
            let state = self.held();
            if state.gone {
                return Err(not_available());
            }
            if let Some(text) = &state.text {
                let size = (state.grid.rows.len(), state.grid.columns);
                return Ok((Arc::clone(text), size));
            }
            (Arc::clone(&state.grid), state.text_key)
        };
        let text = Arc::new(a11y::build(&grid));
        let mut state = self.held();
        if state.gone {
            return Err(not_available());
        }
        if state.text_key == key && Arc::ptr_eq(&state.grid, &grid) {
            state.text = Some(Arc::clone(&text));
        }
        assert!(text.units() >= grid.rows.len().saturating_sub(1));
        Ok((text, (grid.rows.len(), grid.columns)))
    }

    /// The grid size, focus and scale key, for points and properties. Output: rows,
    /// columns, focus and key, or `UIA_E_ELEMENTNOTAVAILABLE` once the view is gone.
    fn shape(&self) -> Result<(usize, usize, bool, u32)> {
        let state = self.held();
        if state.gone {
            return Err(not_available());
        }
        let shape = (
            state.grid.rows.len(),
            state.grid.columns,
            state.focused,
            state.key,
        );
        assert!((100..=400).contains(&shape.3));
        Ok(shape)
    }
}

/// The view's element: a keyboard-focusable Document whose Text pattern reads the screen.
#[implement(IRawElementProviderSimple, ITextProvider)]
pub struct Terminal {
    hwnd: isize,
    access: Arc<Access>,
}

/// A range over the view's text: two UTF-16 endpoints, start at or before end, snapped
/// into the newest text on every call.
#[implement(ITextRangeProvider)]
struct TextRange {
    hwnd: isize,
    access: Arc<Access>,
    span: Mutex<(usize, usize)>,
}

impl Terminal {
    /// The provider of one view. Inputs: the view's window and its access. Output: it.
    pub fn new(hwnd: isize, access: Arc<Access>) -> Terminal {
        Terminal { hwnd, access }
    }

    /// A range over this view's text. Input: the endpoints. Output: the range.
    fn range(&self, span: (usize, usize)) -> ITextRangeProvider {
        range_of(self.hwnd, &self.access, span)
    }
}

/// A new range of a view. Inputs: the window, the access and the endpoints. Output: the
/// range as UI Automation takes it.
fn range_of(hwnd: isize, access: &Arc<Access>, span: (usize, usize)) -> ITextRangeProvider {
    assert!(span.0 <= span.1);
    let range = TextRange {
        hwnd,
        access: Arc::clone(access),
        span: Mutex::new(span),
    };
    range.into()
}

/// A one-dimensional SAFEARRAY of ranges, which UI Automation takes ownership of. Input:
/// the ranges. Output: the array.
fn ranges_array(ranges: &[ITextRangeProvider]) -> Result<*mut SAFEARRAY> {
    let count = u32::try_from(ranges.len()).map_err(|_| Error::from(E_INVALIDARG))?;
    let array = unsafe { SafeArrayCreateVector(VT_UNKNOWN, 0, count) };
    if array.is_null() {
        return Err(Error::from(windows::Win32::Foundation::E_OUTOFMEMORY));
    }
    for (index, range) in ranges.iter().enumerate() {
        let index = i32::try_from(index).expect("bounded by the count");
        if let Err(error) = unsafe { SafeArrayPutElement(array, &index, range.as_raw()) } {
            let _ = unsafe { SafeArrayDestroy(array) };
            return Err(error);
        }
    }
    Ok(array)
}

/// Every text unit UIA defines with the unit it moves by: Format and Word by Line,
/// Paragraph and Page by Document.
const UNITS: [(TextUnit, Unit); 7] = [
    (TextUnit_Character, Unit::Character),
    (TextUnit_Format, Unit::Line),
    (TextUnit_Word, Unit::Line),
    (TextUnit_Line, Unit::Line),
    (TextUnit_Paragraph, Unit::Document),
    (TextUnit_Page, Unit::Document),
    (TextUnit_Document, Unit::Document),
];

/// The unit a UIA text unit moves by. Input: the client's unit. Output: the unit, or
/// `E_INVALIDARG` for a value UIA does not define.
fn unit_of(unit: TextUnit) -> Result<Unit> {
    UNITS
        .iter()
        .find(|(defined, _)| *defined == unit)
        .map(|(_, moves)| *moves)
        .ok_or_else(|| Error::from(E_INVALIDARG))
}

impl IRawElementProviderSimple_Impl for Terminal_Impl {
    fn ProviderOptions(&self) -> Result<ProviderOptions> {
        Ok(ProviderOptions_ServerSideProvider)
    }

    fn GetPatternProvider(&self, patternid: UIA_PATTERN_ID) -> Result<IUnknown> {
        self.access.shape()?;
        if patternid != UIA_TextPatternId {
            return Err(Error::empty());
        }
        let text: ITextProvider = self.to_interface();
        text.cast()
    }

    fn GetPropertyValue(&self, propertyid: UIA_PROPERTY_ID) -> Result<VARIANT> {
        let (_, _, focused, _) = self.access.shape()?;
        let value = if propertyid == UIA_ControlTypePropertyId {
            VARIANT::from(UIA_DocumentControlTypeId.0)
        } else if propertyid == UIA_NamePropertyId {
            VARIANT::from(NAME)
        } else if propertyid == UIA_IsKeyboardFocusablePropertyId {
            VARIANT::from(true)
        } else if propertyid == UIA_HasKeyboardFocusPropertyId {
            VARIANT::from(focused)
        } else {
            VARIANT::default()
        };
        Ok(value)
    }

    fn HostRawElementProvider(&self) -> Result<IRawElementProviderSimple> {
        self.access.shape()?;
        unsafe { UiaHostProviderFromHwnd(HWND(self.hwnd as *mut core::ffi::c_void)) }
    }
}

impl ITextProvider_Impl for Terminal_Impl {
    fn GetSelection(&self) -> Result<*mut SAFEARRAY> {
        let marked = self.access.text()?.marked();
        ranges_array(&[self.range(marked)])
    }

    fn GetVisibleRanges(&self) -> Result<*mut SAFEARRAY> {
        let units = self.access.text()?.units();
        ranges_array(&[self.range((0, units))])
    }

    fn RangeFromChild(
        &self,
        _childelement: Ref<IRawElementProviderSimple>,
    ) -> Result<ITextRangeProvider> {
        self.access.shape()?;
        Err(Error::from(E_INVALIDARG))
    }

    fn RangeFromPoint(&self, point: &UiaPoint) -> Result<ITextRangeProvider> {
        let (_, _, _, key) = self.access.shape()?;
        let (text, (rows, columns)) = self.access.read()?;
        let Some((origin, cell)) = screen_cells(self.hwnd, self.access.id, key) else {
            return Ok(self.range((0, 0)));
        };
        if rows == 0 || columns == 0 {
            return Ok(self.range((0, 0)));
        }
        let at = (point.x - origin.0, point.y - origin.1);
        let (row, column) = cell_near(pixels(cell), at, rows, columns);
        let index = text.index_at(row, column);
        Ok(self.range((index, index)))
    }

    fn DocumentRange(&self) -> Result<ITextRangeProvider> {
        let units = self.access.text()?.units();
        Ok(self.range((0, units)))
    }

    fn SupportedTextSelection(&self) -> Result<SupportedTextSelection> {
        self.access.shape()?;
        Ok(SupportedTextSelection_Single)
    }
}

/// The geometry `a11y` measures device pixels with: one cell's pixel size at scale one.
/// Input: the cell size in pixels. Output: the geometry.
fn pixels(cell: (f64, f64)) -> Geometry {
    assert!(cell.0 > 0.0 && cell.1 > 0.0);
    Geometry {
        cell,
        pad: 0.0,
        scale: 1.0,
    }
}

/// The view's client origin on the screen and its cell size, both in device pixels, read
/// from the window and the render side's posted cell. Inputs: the window, the view id and
/// its scale key. Output: origin and cell, or none before the first cell or for a window
/// Windows cannot place.
fn screen_cells(hwnd: isize, id: u64, key: u32) -> Option<((f64, f64), (f64, f64))> {
    let points = BOARD.cell(id)?;
    let scale = f64::from(key) / 100.0;
    let cell = (points.0 * scale, points.1 * scale);
    let sized = cell.0.is_finite() && cell.1.is_finite() && cell.0 > 0.0 && cell.1 > 0.0;
    if !sized {
        return None;
    }
    let mut origin = windows_sys::Win32::Foundation::POINT { x: 0, y: 0 };
    let window = hwnd as windows_sys::Win32::Foundation::HWND;
    let placed = unsafe { windows_sys::Win32::Graphics::Gdi::ClientToScreen(window, &mut origin) };
    if placed == 0 {
        return None;
    }
    Some(((f64::from(origin.x), f64::from(origin.y)), cell))
}

impl TextRange {
    /// This range's endpoints and the newest text, the endpoints snapped into it. Output:
    /// the text and the span, or `UIA_E_ELEMENTNOTAVAILABLE` once the view is gone.
    fn fresh(&self) -> Result<(Arc<Text>, (usize, usize))> {
        let span = *self
            .span
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let text = self.access.text()?;
        let start = text.snap(span.0);
        let end = text.snap(span.1).max(start);
        assert!(end <= text.units());
        Ok((text, (start, end)))
    }

    /// Sets this range's endpoints. Input: the span, start at or before end.
    fn set(&self, span: (usize, usize)) {
        assert!(span.0 <= span.1);
        *self
            .span
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = span;
    }

    /// The endpoints of a range a client passed in, which must be a range of this view.
    /// Input: the client's range. Output: its span snapped into `text`, or `E_INVALIDARG`
    /// for a null pointer, a range that is not ours or a range of another view.
    fn other(&self, range: Ref<ITextRangeProvider>, text: &Text) -> Result<(usize, usize)> {
        let range = range.ok().map_err(|_| Error::from(E_INVALIDARG))?;
        let theirs = range
            .cast_object_ref::<TextRange>()
            .map_err(|_| Error::from(E_INVALIDARG))?;
        if !Arc::ptr_eq(&theirs.access, &self.access) {
            return Err(Error::from(E_INVALIDARG));
        }
        let span = *theirs
            .span
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let start = text.snap(span.0);
        Ok((start, text.snap(span.1).max(start)))
    }
}

/// One endpoint of a span. Inputs: the span and the endpoint. Output: the index, or
/// `E_INVALIDARG` for a value UIA does not define.
fn endpoint(span: (usize, usize), which: TextPatternRangeEndpoint) -> Result<usize> {
    if which == TextPatternRangeEndpoint_Start {
        Ok(span.0)
    } else if which == TextPatternRangeEndpoint_End {
        Ok(span.1)
    } else {
        Err(Error::from(E_INVALIDARG))
    }
}

/// A span with one endpoint moved; the other follows when they would cross. Inputs: the
/// span, the endpoint and its new index. Output: the span, or `E_INVALIDARG`.
fn with_endpoint(
    span: (usize, usize),
    which: TextPatternRangeEndpoint,
    at: usize,
) -> Result<(usize, usize)> {
    let moved = if which == TextPatternRangeEndpoint_Start {
        (at, span.1.max(at))
    } else if which == TextPatternRangeEndpoint_End {
        (span.0.min(at), at)
    } else {
        return Err(Error::from(E_INVALIDARG));
    };
    assert!(moved.0 <= moved.1);
    Ok(moved)
}

impl ITextRangeProvider_Impl for TextRange_Impl {
    fn Clone(&self) -> Result<ITextRangeProvider> {
        let (_, span) = self.fresh()?;
        Ok(range_of(self.hwnd, &self.access, span))
    }

    fn Compare(&self, range: Ref<ITextRangeProvider>) -> Result<BOOL> {
        let (text, span) = self.fresh()?;
        Ok((self.other(range, &text)? == span).into())
    }

    fn CompareEndpoints(
        &self,
        endpoint_mine: TextPatternRangeEndpoint,
        targetrange: Ref<ITextRangeProvider>,
        targetendpoint: TextPatternRangeEndpoint,
    ) -> Result<i32> {
        let (text, span) = self.fresh()?;
        let mine = endpoint(span, endpoint_mine)?;
        let theirs = endpoint(self.other(targetrange, &text)?, targetendpoint)?;
        Ok(match mine.cmp(&theirs) {
            std::cmp::Ordering::Less => -1,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => 1,
        })
    }

    fn ExpandToEnclosingUnit(&self, unit: TextUnit) -> Result<()> {
        let unit = unit_of(unit)?;
        let (text, span) = self.fresh()?;
        self.set(text.enclosing(span.0, unit));
        Ok(())
    }

    fn FindAttribute(
        &self,
        _attributeid: UIA_TEXTATTRIBUTE_ID,
        _val: &VARIANT,
        _backward: BOOL,
    ) -> Result<ITextRangeProvider> {
        self.fresh()?;
        Err(Error::empty())
    }

    fn FindText(
        &self,
        _text: &BSTR,
        _backward: BOOL,
        _ignorecase: BOOL,
    ) -> Result<ITextRangeProvider> {
        self.fresh()?;
        Err(Error::empty())
    }

    fn GetAttributeValue(&self, _attributeid: UIA_TEXTATTRIBUTE_ID) -> Result<VARIANT> {
        self.fresh()?;
        let unsupported = unsafe { UiaGetReservedNotSupportedValue() }?;
        Ok(VARIANT::from(unsupported))
    }

    fn GetBoundingRectangles(&self) -> Result<*mut SAFEARRAY> {
        let (text, span) = self.fresh()?;
        let (_, _, _, key) = self.access.shape()?;
        let lines = text.lines_of(span.0, span.1);
        let placed = screen_cells(self.hwnd, self.access.id, key);
        let mut sides = Vec::with_capacity(lines.len() * 4);
        if let Some((origin, cell)) = placed {
            for (row, first, past) in lines {
                let (x, y, width, height) = a11y::cell_rect(pixels(cell), row, first, past - first);
                sides.extend([origin.0 + x, origin.1 + y, width, height]);
            }
        }
        doubles_array(&sides)
    }

    fn GetEnclosingElement(&self) -> Result<IRawElementProviderSimple> {
        self.fresh()?;
        Ok(Terminal::new(self.hwnd, Arc::clone(&self.access)).into())
    }

    fn GetText(&self, maxlength: i32) -> Result<BSTR> {
        let (text, span) = self.fresh()?;
        let read = text.string_for_range((span.0, span.1 - span.0));
        let string = read.map(|(string, _)| string).unwrap_or_default();
        let mut wide: Vec<u16> = string.encode_utf16().collect();
        let keep = usize::try_from(maxlength).map_or(wide.len(), |keep| keep.min(wide.len()));
        if keep < wide.len() {
            let cuts_pair = keep > 0 && (0xD800..0xDC00).contains(&wide[keep - 1]);
            wide.truncate(keep - usize::from(cuts_pair));
        }
        Ok(BSTR::from_wide(&wide))
    }

    fn Move(&self, unit: TextUnit, count: i32) -> Result<i32> {
        let unit = unit_of(unit)?;
        let (text, span) = self.fresh()?;
        if count == 0 {
            return Ok(0);
        }
        if span.0 == span.1 {
            let (at, moved) = text.moved(span.0, unit, count);
            self.set((at, at));
            return Ok(moved);
        }
        let start = text.enclosing(span.0, unit).0;
        let (mut at, mut moved) = text.moved(start, unit, count);
        if at == text.units() && moved > 0 {
            at = text.moved(at, unit, -1).0;
            moved -= 1;
        }
        self.set(text.enclosing(at, unit));
        Ok(moved)
    }

    fn MoveEndpointByUnit(
        &self,
        which: TextPatternRangeEndpoint,
        unit: TextUnit,
        count: i32,
    ) -> Result<i32> {
        let unit = unit_of(unit)?;
        let (text, span) = self.fresh()?;
        let (at, moved) = text.moved(endpoint(span, which)?, unit, count);
        self.set(with_endpoint(span, which, at)?);
        Ok(moved)
    }

    fn MoveEndpointByRange(
        &self,
        which: TextPatternRangeEndpoint,
        targetrange: Ref<ITextRangeProvider>,
        targetendpoint: TextPatternRangeEndpoint,
    ) -> Result<()> {
        let (text, span) = self.fresh()?;
        let at = endpoint(self.other(targetrange, &text)?, targetendpoint)?;
        self.set(with_endpoint(span, which, at)?);
        Ok(())
    }

    fn Select(&self) -> Result<()> {
        Err(refused())
    }

    fn AddToSelection(&self) -> Result<()> {
        Err(refused())
    }

    fn RemoveFromSelection(&self) -> Result<()> {
        Err(refused())
    }

    fn ScrollIntoView(&self, _aligntotop: BOOL) -> Result<()> {
        self.fresh()?;
        Ok(())
    }

    fn GetChildren(&self) -> Result<*mut SAFEARRAY> {
        self.fresh()?;
        ranges_array(&[])
    }
}

/// A one-dimensional SAFEARRAY of doubles, four per rectangle (left, top, width, height),
/// which UI Automation takes ownership of. Input: the doubles. Output: the array.
fn doubles_array(values: &[f64]) -> Result<*mut SAFEARRAY> {
    assert_eq!(values.len() % 4, 0);
    let count = u32::try_from(values.len()).map_err(|_| Error::from(E_INVALIDARG))?;
    let array = unsafe { SafeArrayCreateVector(VT_R8, 0, count) };
    if array.is_null() {
        return Err(Error::from(windows::Win32::Foundation::E_OUTOFMEMORY));
    }
    for (index, value) in values.iter().enumerate() {
        let index = i32::try_from(index).expect("bounded by the count");
        let at = (value as *const f64).cast();
        if let Err(error) = unsafe { SafeArrayPutElement(array, &index, at) } {
            let _ = unsafe { SafeArrayDestroy(array) };
            return Err(error);
        }
    }
    Ok(array)
}

/// The provider a view's window answers UI Automation with, made on its first request and
/// held until the window goes. Window thread only.
pub struct Root {
    access: Arc<Access>,
    held: std::cell::RefCell<Option<IRawElementProviderSimple>>,
}

impl Root {
    /// The root of one view. Input: the view's access. Output: the root, no provider yet.
    pub fn new(access: Arc<Access>) -> Root {
        Root {
            access,
            held: std::cell::RefCell::new(None),
        }
    }

    /// The view's access, for its window thread's posts. Output: the access.
    pub fn access(&self) -> &Arc<Access> {
        &self.access
    }

    /// Answers WM_GETOBJECT for the root object with the view's provider; another object id
    /// goes to the default procedure, and so does a request after the view ended. Inputs:
    /// the window and the message's parameters. Output: the result to return, or none.
    pub fn answer(&self, hwnd: isize, wparam: usize, lparam: isize) -> Option<isize> {
        if lparam as i32 != UiaRootObjectId || self.access.shape().is_err() {
            return None;
        }
        let provider = self
            .held
            .borrow_mut()
            .get_or_insert_with(|| Terminal::new(hwnd, Arc::clone(&self.access)).into())
            .clone();
        let window = HWND(hwnd as *mut core::ffi::c_void);
        let result = unsafe {
            UiaReturnRawElementProvider(window, WPARAM(wparam), LPARAM(lparam), &provider)
        };
        Some(result.0)
    }

    /// Raises UI Automation's text and selection events for what a post changed, only when
    /// a client listens and the provider exists; the provider is cloned out first, since UI
    /// Automation may send WM_GETOBJECT during the raise. Input: what changed.
    pub fn raise(&self, changed: Changed) {
        let held = self.held.borrow().clone();
        let Some(provider) = held else {
            return;
        };
        if !unsafe { UiaClientsAreListening() }.as_bool() {
            return;
        }
        let events: [(bool, UIA_EVENT_ID); 2] = [
            (changed.text, UIA_Text_TextChangedEventId),
            (changed.selection, UIA_Text_TextSelectionChangedEventId),
        ];
        for (owed, event) in events {
            if owed {
                let _ = unsafe { UiaRaiseAutomationEvent(&provider, event) };
            }
        }
    }

    /// Ends the view's access on WM_DESTROY: later calls answer `UIA_E_ELEMENTNOTAVAILABLE`,
    /// the provider is disconnected from its clients unless this runs inside another
    /// thread's SendMessage, which UI Automation refuses, and the window's event-map entries
    /// are released. Inputs: the window and whether a cross-thread send is in progress.
    pub fn end(&self, hwnd: isize, in_send: bool) {
        self.access.end();
        let provider = self.held.borrow_mut().take();
        if let Some(provider) = provider.filter(|_| !in_send) {
            let _ = unsafe { UiaDisconnectProvider(&provider) };
        }
        let window = HWND(hwnd as *mut core::ffi::c_void);
        let none: Option<&IRawElementProviderSimple> = None;
        unsafe { UiaReturnRawElementProvider(window, WPARAM(0), LPARAM(0), none) };
        assert!(self.held.borrow().is_none());
        assert!(self.access.shape().is_err());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_term::screen::GridSize;
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::term::{Config, Term};
    use alacritty_terminal::vte::ansi::Processor;

    fn term(bytes: &[u8]) -> Term<VoidListener> {
        let size = GridSize {
            columns: 10,
            lines: 3,
        };
        let mut term = Term::new(Config::default(), &size, VoidListener);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut term, bytes);
        term
    }

    fn posted(access: &Access, bytes: &[u8]) -> Changed {
        let term = term(bytes);
        access.post(a11y::visible(&term), a11y::text_key(&term))
    }

    fn provider(access: &Arc<Access>) -> ITextProvider {
        Terminal::new(0, Arc::clone(access)).into()
    }

    fn read(range: &ITextRangeProvider) -> String {
        let read = unsafe { range.GetText(-1) }.expect("the range's text");
        String::from_utf16(&read).expect("UTF-16 text")
    }

    fn code(result: Result<impl Sized>) -> HRESULT {
        result.err().expect("an error").code()
    }

    #[test]
    fn a_document_range_reads_moves_and_expands_by_lines_and_characters() {
        let access = Access::new(1, 100);
        let changed = posted(&access, b"ab\r\ncd");
        assert_eq!(
            changed,
            Changed {
                text: true,
                selection: true
            }
        );
        let document = unsafe { provider(&access).DocumentRange() }.expect("the document");
        assert_eq!(read(&document), "ab\ncd\n");
        let line = unsafe { document.Clone() }.expect("a clone");
        unsafe { line.ExpandToEnclosingUnit(TextUnit_Line) }.expect("expanded");
        assert_eq!(read(&line), "ab\n");
        assert_eq!(unsafe { line.Move(TextUnit_Line, 1) }.expect("moved"), 1);
        assert_eq!(read(&line), "cd\n");
        let back = unsafe {
            line.MoveEndpointByUnit(TextPatternRangeEndpoint_End, TextUnit_Character, -1)
        };
        assert_eq!(back.expect("moved back"), -1);
        assert_eq!(read(&line), "cd");
        let after = unsafe {
            line.CompareEndpoints(
                TextPatternRangeEndpoint_Start,
                &document,
                TextPatternRangeEndpoint_Start,
            )
        };
        assert_eq!(after.expect("compared"), 1);
        assert!(
            !unsafe { line.Compare(&document) }
                .expect("compared")
                .as_bool()
        );
        assert_eq!(
            String::from_utf16(&unsafe { line.GetText(1) }.expect("one")).unwrap(),
            "c"
        );
        assert_eq!(
            code(unsafe { line.Select() }),
            HRESULT(UIA_E_INVALIDOPERATION as i32)
        );
        assert_eq!(posted(&access, b"ab\r\ncd"), Changed::default());
    }

    #[test]
    fn a_range_of_another_view_or_of_no_view_is_refused_as_an_argument() {
        let mine = Access::new(1, 100);
        let theirs = Access::new(2, 100);
        posted(&mine, b"ab");
        posted(&theirs, b"ab");
        let document = unsafe { provider(&mine).DocumentRange() }.expect("mine");
        let other = unsafe { provider(&theirs).DocumentRange() }.expect("theirs");
        assert_eq!(code(unsafe { document.Compare(&other) }), E_INVALIDARG);
        let crossed = unsafe {
            document.CompareEndpoints(
                TextPatternRangeEndpoint_Start,
                &other,
                TextPatternRangeEndpoint_End,
            )
        };
        assert_eq!(code(crossed), E_INVALIDARG);
        let none: Option<&ITextRangeProvider> = None;
        let moved = unsafe {
            document.MoveEndpointByRange(
                TextPatternRangeEndpoint_End,
                none,
                TextPatternRangeEndpoint_Start,
            )
        };
        assert_eq!(code(moved), E_INVALIDARG);
        assert_eq!(
            code(unsafe { document.ExpandToEnclosingUnit(TextUnit(99)) }),
            E_INVALIDARG
        );
        assert_eq!(read(&document), "ab\n\n");
    }

    #[test]
    fn a_range_held_past_its_view_answers_element_not_available() {
        let access = Access::new(1, 100);
        posted(&access, b"ab");
        let terminal = provider(&access);
        let document = unsafe { terminal.DocumentRange() }.expect("the document");
        access.end();
        let gone = HRESULT(UIA_E_ELEMENTNOTAVAILABLE as i32);
        assert_eq!(code(unsafe { document.GetText(-1) }), gone);
        assert_eq!(code(unsafe { document.Move(TextUnit_Character, 1) }), gone);
        assert_eq!(code(unsafe { terminal.DocumentRange() }), gone);
        let simple: IRawElementProviderSimple = terminal.cast().expect("the element");
        assert_eq!(
            code(unsafe { simple.GetPropertyValue(UIA_NamePropertyId) }),
            gone
        );
    }

    #[test]
    fn endpoints_snap_into_a_shorter_text_and_a_cut_keeps_surrogate_pairs_whole() {
        let access = Access::new(1, 100);
        posted(&access, b"abcdef\r\nghij");
        let document = unsafe { provider(&access).DocumentRange() }.expect("the document");
        let tail = unsafe { document.Clone() }.expect("a clone");
        let moved = unsafe {
            tail.MoveEndpointByUnit(TextPatternRangeEndpoint_Start, TextUnit_Character, 8)
        };
        assert_eq!(moved.expect("moved"), 8);
        assert_eq!(read(&tail), "hij\n");
        posted(&access, "😀a".as_bytes());
        assert_eq!(read(&document), "😀a\n\n");
        assert_eq!(read(&tail), "");
        let cut = unsafe { document.GetText(1) }.expect("cut");
        assert_eq!(cut.len(), 0);
        let element: IRawElementProviderSimple = provider(&access).cast().expect("the element");
        let name = unsafe { element.GetPropertyValue(UIA_NamePropertyId) }.expect("the name");
        assert_eq!(name.to_string(), NAME);
    }
}
