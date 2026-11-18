use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use gtk::gdk::keys::Key;
use gtk::gdk::keys::constants as key;
use gtk::gdk::{self, EventKey, EventType, ModifierType, ScrollDirection};
use gtk::glib::{Propagation, SignalHandlerId, WeakRef};
use gtk::prelude::*;

use super::clip::{Board, Clip};
use super::keys;
use super::signals::Handlers;
use super::surface::{Rect, page_bounds};
use crate::native_term::event::{MouseKind, Sink, ViewEvent};
use crate::native_term::pane::Input;

/// The modifiers a chord is read against; Lock, Num Lock, the pointer buttons and the
/// input method's forward bit never change a chord.
const CHORD_MODIFIERS: ModifierType = ModifierType::SHIFT_MASK
    .union(ModifierType::CONTROL_MASK)
    .union(ModifierType::MOD1_MASK)
    .union(ModifierType::MOD4_MASK)
    .union(ModifierType::MOD5_MASK)
    .union(ModifierType::SUPER_MASK)
    .union(ModifierType::HYPER_MASK)
    .union(ModifierType::META_MASK);

/// The largest input method commit a view passes on, in bytes; a larger one is dropped.
pub const COMMIT_BYTES_MAX: usize = 1 << 20;

/// The view command a key chord makes, by the Linux terminal convention: Ctrl+Shift+C
/// copies, Ctrl+Shift+V pastes, Ctrl+Shift+A selects all and Shift+Insert pastes. Inputs:
/// the key and the modifier state. Output: the command, or none for any other key.
pub fn chord(keyval: Key, state: ModifierType) -> Option<ViewEvent> {
    let held = state & CHORD_MODIFIERS;
    let command = if held == ModifierType::CONTROL_MASK | ModifierType::SHIFT_MASK {
        match keyval.to_lower() {
            key::c => ViewEvent::Copy,
            key::v => ViewEvent::Paste,
            key::a => ViewEvent::SelectAll,
            _ => return None,
        }
    } else if held == ModifierType::SHIFT_MASK && matches!(keyval, key::Insert | key::KP_Insert) {
        ViewEvent::Paste
    } else {
        return None;
    };
    assert!(matches!(
        command,
        ViewEvent::Copy | ViewEvent::Paste | ViewEvent::SelectAll
    ));
    Some(command)
}

/// The input one input method commit makes: its UTF-8 bytes as one text input, the one
/// path text takes (spike run 3: IBus commits every plain letter). Input: the committed
/// text. Output: the input, or none for empty text or text over `COMMIT_BYTES_MAX` bytes.
pub fn commit_input(text: &str) -> Option<Input> {
    if text.is_empty() || text.len() > COMMIT_BYTES_MAX {
        return None;
    }
    Some(Input::Text(text.as_bytes().to_vec()))
}

/// The event an input method commit makes for a view: its text input while the view is
/// active, none otherwise, so a commit that comes after a cover, a focus loss or a
/// close never lands. Inputs: the view's focus and the committed text. Output: the event.
pub fn commit_event(focus: Focus, text: &str) -> Option<ViewEvent> {
    if !focus.active() {
        return None;
    }
    commit_input(text).map(ViewEvent::Key)
}

/// The event a middle click's PRIMARY read makes when it comes back: a paste input marked
/// as the primary selection's, which the pane flattens for an app without bracketed paste,
/// and only while the view is still placed and uncovered. Inputs: the view's focus, whether
/// it is placed and the text read. Output: the event.
pub fn primary_paste(focus: Focus, placed: bool, text: Option<String>) -> Option<ViewEvent> {
    let _ = focus;
    if !placed {
        return None;
    }
    let text = text.filter(|text| !text.is_empty())?;
    Some(ViewEvent::Key(Input::Paste {
        text,
        primary: true,
    }))
}

/// Where keyboard focus stands for one view: whether it holds native focus, whether its
/// toplevel has keyboard focus, whether a page element covers it and whether its rectangle
/// shows inside the page. Keys go to the view only while it is active: native, in a focused
/// toplevel, uncovered and on screen. A cover or going off screen drops native focus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Focus {
    native: bool,
    toplevel: bool,
    covered: bool,
    shown: bool,
}

impl Focus {
    /// A new view's focus: not native, uncovered and not on screen. Input: whether the
    /// toplevel has keyboard focus. Output: the focus.
    pub const fn new(toplevel: bool) -> Focus {
        Focus {
            native: false,
            toplevel,
            covered: false,
            shown: false,
        }
    }

    /// Whether keys go to the view. Output: the answer.
    pub fn active(self) -> bool {
        self.native && self.toplevel && !self.covered
    }

    /// Whether the view holds native focus, whatever its toplevel's. Output: the answer.
    pub fn is_native(self) -> bool {
        self.native
    }

    /// Whether a page element covers the view. Output: the answer.
    pub fn is_covered(self) -> bool {
        self.covered
    }

    /// Gives or takes native focus; a covered view or one off screen refuses it. Input: on
    /// or off. Output: the new activity when it changed.
    pub fn native(&mut self, on: bool) -> Option<bool> {
        self.change(|focus| focus.native = on && !focus.covered && focus.shown)
    }

    /// Notes the toplevel gaining or losing keyboard focus. Input: on or off. Output: the
    /// new activity when it changed.
    pub fn toplevel(&mut self, on: bool) -> Option<bool> {
        self.change(|focus| focus.toplevel = on)
    }

    /// Covers or uncovers the view; covering drops native focus. Input: covered or not.
    /// Output: the new activity when it changed.
    pub fn cover(&mut self, covered: bool) -> Option<bool> {
        self.change(|focus| {
            focus.covered = covered;
            focus.native &= !covered;
        })
    }

    /// Notes whether any part of the view's rectangle shows inside the page; going off
    /// screen drops native focus, which only a click or the host gives back. Input: shown
    /// or not. Output: the new activity when it changed.
    pub fn shown(&mut self, shown: bool) -> Option<bool> {
        self.change(|focus| {
            focus.shown = shown;
            focus.native &= shown;
        })
    }

    fn change(&mut self, edit: impl FnOnce(&mut Focus)) -> Option<bool> {
        let before = self.active();
        edit(self);
        assert!(!(self.native && (self.covered || !self.shown)));
        let after = self.active();
        (before != after).then_some(after)
    }
}

/// Cancels the context's preedit and takes its focus. Input: the context. Output: none.
fn release(im: &gtk::IMMulticontext) {
    im.reset();
    im.focus_out();
}

/// The most clicks a press reports: 1 places the cursor, 2 a word, 3 a line, as on macOS.
const CLICKS_MAX: u8 = 3;

/// Points one GDK smooth scroll unit stands for on a touchpad: GDK's Wayland backend
/// divides the compositor's pixels by ten.
pub const TOUCHPAD_POINTS_PER_UNIT: f64 = 10.0;

/// The double-click limits from GTK's settings: the most milliseconds and points between
/// two left presses that make one more click.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DoubleClick {
    pub time_ms: u32,
    pub distance: f64,
}

impl DoubleClick {
    /// The limits from GTK's default settings, bounded, or GTK's own defaults (400 ms and
    /// 5 points) without settings. Output: the limits.
    fn from_settings() -> DoubleClick {
        let (time, distance) = gtk::Settings::default()
            .map(|settings| {
                (
                    settings.gtk_double_click_time(),
                    settings.gtk_double_click_distance(),
                )
            })
            .unwrap_or((400, 5));
        let double = DoubleClick {
            time_ms: u32::try_from(time.clamp(1, 5_000)).unwrap_or(400),
            distance: f64::from(distance.clamp(0, 100)),
        };
        assert!(double.time_ms >= 1 && double.distance >= 0.0);
        double
    }
}

/// Where one pointer event goes: on to the page; to the view, which takes native focus
/// first when `focus` is set and then hands `event`, if any, to its host; or to the view
/// as a middle click, which reads PRIMARY and pastes it.
#[derive(Debug, PartialEq)]
pub enum Route {
    Page,
    Primary,
    View {
        focus: bool,
        event: Option<ViewEvent>,
    },
}

/// The pointer buttons GDK's modifier state carries.
const BUTTONS: ModifierType = ModifierType::BUTTON1_MASK
    .union(ModifierType::BUTTON2_MASK)
    .union(ModifierType::BUTTON3_MASK)
    .union(ModifierType::BUTTON4_MASK)
    .union(ModifierType::BUTTON5_MASK);

/// The modifier bit of a pointer button, for the buttons GDK's state carries. Input: the
/// button. Output: its bit, or none above the fifth button.
fn button_bit(button: u32) -> ModifierType {
    match button {
        1 => ModifierType::BUTTON1_MASK,
        2 => ModifierType::BUTTON2_MASK,
        3 => ModifierType::BUTTON3_MASK,
        4 => ModifierType::BUTTON4_MASK,
        5 => ModifierType::BUTTON5_MASK,
        _ => ModifierType::empty(),
    }
}

/// One left press, kept to count the next one.
#[derive(Clone, Copy, Debug)]
struct Click {
    time: u32,
    at: (f64, f64),
    count: u8,
}

/// Pointer routing for one view, in the toplevel's logical coordinates. A press inside
/// the placed, uncovered rectangle starts a press the view holds until that button's
/// release, wherever the pointer goes; any other event outside a held press goes to the
/// page, and so does everything while the view is covered or unplaced.
#[derive(Debug, Default)]
pub struct Pointer {
    rect: Option<Rect>,
    pressing: Option<u32>,
    clicks: u8,
    last: Option<Click>,
}

impl Pointer {
    /// An unplaced router. Output: it.
    pub const fn new() -> Pointer {
        Pointer {
            rect: None,
            pressing: None,
            clicks: 0,
            last: None,
        }
    }

    /// Places the view's rectangle, or unplaces it, which drops a held press. Input: the
    /// rectangle in the toplevel's logical coordinates. Output: none.
    pub fn place(&mut self, rect: Option<Rect>) {
        self.rect = rect;
        if rect.is_none() {
            self.pressing = None;
        }
    }

    /// Drops a held press, as a cover does, so the rest of it goes to the page. Output:
    /// none.
    pub fn cover(&mut self) {
        self.pressing = None;
    }

    /// Whether the view has a rectangle. Output: the answer.
    pub fn is_placed(&self) -> bool {
        self.rect.is_some()
    }

    /// Routes a button press: the left button presses with a click count and takes focus,
    /// the right opens the menu, the middle pastes PRIMARY, others are held without events;
    /// with another button down the press stays with whoever holds that one. Inputs: the
    /// button, the point, the time in ms, GDK's modifier state, whether the view is covered
    /// and the double-click limits. Output: the route.
    pub fn press(
        &mut self,
        button: u32,
        at: (f64, f64),
        time: u32,
        state: ModifierType,
        covered: bool,
        double: DoubleClick,
    ) -> Route {
        let option = state.contains(ModifierType::MOD1_MASK);
        if self.pressing.is_some() {
            return Route::View {
                focus: false,
                event: None,
            };
        }
        let others = state & BUTTONS & !button_bit(button);
        let Some(point) = self.inside(at).filter(|_| !covered && others.is_empty()) else {
            return Route::Page;
        };
        self.pressing = Some(button);
        if button != 1 {
            self.last = None;
        }
        if button == 2 {
            return Route::Primary;
        }
        let event = match button {
            1 => {
                self.clicks = self.count(time, at, double);
                Some(ViewEvent::Mouse {
                    kind: MouseKind::Press,
                    point,
                    option,
                    clicks: self.clicks,
                })
            }
            3 => Some(ViewEvent::Menu { point }),
            _ => None,
        };
        Route::View {
            focus: button == 1,
            event,
        }
    }

    /// Routes GDK's extra double and triple press events, which follow a press the
    /// router already counted. Output: held while a press is held, else the page's.
    pub fn repeat(&self) -> Route {
        match self.pressing {
            Some(_) => Route::View {
                focus: false,
                event: None,
            },
            None => Route::Page,
        }
    }

    /// Routes pointer motion: a drag while the left button is held, nothing while another
    /// is held, the page's otherwise. Inputs: the point and whether Alt is held. Output:
    /// the route.
    pub fn motion(&mut self, at: (f64, f64), option: bool) -> Route {
        match self.pressing {
            None => Route::Page,
            Some(1) => Route::View {
                focus: false,
                event: self.mouse(MouseKind::Drag, at, option),
            },
            Some(_) => Route::View {
                focus: false,
                event: None,
            },
        }
    }

    /// Routes a button release: the held button's release ends the press. Inputs: the
    /// button, the point and whether Alt is held. Output: the route.
    pub fn release(&mut self, button: u32, at: (f64, f64), option: bool) -> Route {
        let Some(held) = self.pressing else {
            return Route::Page;
        };
        if held != button {
            return Route::View {
                focus: false,
                event: None,
            };
        }
        self.pressing = None;
        let event = (button == 1)
            .then(|| self.mouse(MouseKind::Release, at, option))
            .flatten();
        Route::View {
            focus: false,
            event,
        }
    }

    /// Routes a vertical scroll inside the uncovered rectangle: lines for a mouse,
    /// points for a touchpad, up positive as on macOS. Inputs: the point, GDK's vertical
    /// delta with down positive, whether a touchpad made it and whether the view is
    /// covered. Output: the route; a zero or non-finite delta is held without an event.
    pub fn scroll(&self, at: (f64, f64), dy: f64, touchpad: bool, covered: bool) -> Route {
        let Some(point) = self.inside(at).filter(|_| !covered) else {
            return Route::Page;
        };
        if !dy.is_finite() || dy == 0.0 {
            return Route::View {
                focus: false,
                event: None,
            };
        }
        let (delta, precise) = if touchpad {
            (-dy * TOUCHPAD_POINTS_PER_UNIT, true)
        } else {
            (-dy, false)
        };
        Route::View {
            focus: false,
            event: Some(ViewEvent::Wheel {
                point,
                delta,
                precise,
            }),
        }
    }

    fn local(&self, at: (f64, f64)) -> Option<(f64, f64)> {
        let rect = self.rect?;
        let point = (at.0 - f64::from(rect.x), at.1 - f64::from(rect.y));
        (point.0.is_finite() && point.1.is_finite()).then_some(point)
    }

    fn inside(&self, at: (f64, f64)) -> Option<(f64, f64)> {
        let rect = self.rect?;
        let point = self.local(at)?;
        let fits = (0.0..f64::from(rect.w)).contains(&point.0)
            && (0.0..f64::from(rect.h)).contains(&point.1);
        fits.then_some(point)
    }

    fn mouse(&self, kind: MouseKind, at: (f64, f64), option: bool) -> Option<ViewEvent> {
        let point = self.local(at)?;
        assert!((1..=CLICKS_MAX).contains(&self.clicks));
        Some(ViewEvent::Mouse {
            kind,
            point,
            option,
            clicks: self.clicks,
        })
    }

    fn count(&mut self, time: u32, at: (f64, f64), double: DoubleClick) -> u8 {
        let count = match self.last {
            Some(last)
                if time.wrapping_sub(last.time) <= double.time_ms
                    && (at.0 - last.at.0).hypot(at.1 - last.at.1) <= double.distance =>
            {
                (last.count + 1).min(CLICKS_MAX)
            }
            _ => 1,
        };
        assert!((1..=CLICKS_MAX).contains(&count));
        self.last = Some(Click { time, at, count });
        count
    }
}

/// The vertical scroll of a GDK scroll event in GDK's units, down positive, and whether a
/// touchpad made it; a horizontal scroll is zero. GDK gives a smooth scroll no direction,
/// only deltas, so no direction means read the deltas. Input: the event. Output: them.
fn scroll_dy(event: &gdk::Event) -> (f64, bool) {
    let touchpad = event
        .source_device()
        .is_some_and(|device| device.source() == gdk::InputSource::Touchpad);
    let dy = match event.scroll_direction() {
        Some(ScrollDirection::Up) => -1.0,
        Some(ScrollDirection::Down) => 1.0,
        Some(_) => 0.0,
        None => event.scroll_deltas().map_or(0.0, |(_, dy)| dy),
    };
    (dy, touchpad)
}

/// The view of one toplevel that holds native focus: a view taking native focus releases
/// the one before, so keys go to one view per toplevel, whatever order the views' handlers
/// run in.
struct Owner {
    view: RefCell<Weak<Inner>>,
}

thread_local! {
    /// Each toplevel's native focus owner, while a view of that toplevel lives.
    static OWNERS: RefCell<Vec<(WeakRef<gtk::Window>, Weak<Owner>)>> =
        const { RefCell::new(Vec::new()) };
}

/// The native focus owner every view of a toplevel shares. Input: the toplevel. Output:
/// the owner.
fn owner_of(toplevel: &gtk::Window) -> Rc<Owner> {
    OWNERS.with(|owners| {
        let mut owners = owners.borrow_mut();
        owners.retain(|(window, owner)| window.upgrade().is_some() && owner.strong_count() > 0);
        let found = owners
            .iter()
            .find(|(window, _)| window.upgrade().as_ref() == Some(toplevel))
            .and_then(|(_, owner)| owner.upgrade());
        found.unwrap_or_else(|| {
            let owner = Rc::new(Owner {
                view: RefCell::new(Weak::new()),
            });
            owners.push((toplevel.downgrade(), Rc::downgrade(&owner)));
            owner
        })
    })
}

/// The state one view's signal handlers share.
struct Inner {
    id: u64,
    sink: Sink,
    im: gtk::IMMulticontext,
    focus: Cell<Focus>,
    toplevel: gtk::Window,
    area: gtk::Widget,
    owner: Rc<Owner>,
    page: RefCell<Option<WeakRef<gtk::Widget>>>,
    pointer: RefCell<Pointer>,
    double: DoubleClick,
    clip: Rc<Clip>,
}

impl Inner {
    fn send(&self, event: ViewEvent) {
        assert!(self.id > 0);
        (self.sink)(self.id, event);
    }

    /// Applies a focus change: taking native focus releases the toplevel's other native
    /// view, GTK focus leaves the page before the input method takes focus, and the focus
    /// is stored before a lost activity resets the input method. Inputs: the change and
    /// whether losing native focus gives GTK focus back. Output: the widget to give it back
    /// to, when native focus was lost without giving it back.
    fn moving(
        self: &Rc<Self>,
        change: impl FnOnce(&mut Focus) -> Option<bool>,
        restore: bool,
    ) -> Option<WeakRef<gtk::Widget>> {
        let before = self.focus.get();
        let mut next = before;
        let changed = change(&mut next);
        if !before.is_native() && next.is_native() {
            let handed = self.supersede();
            self.take_page_focus();
            if self.page.borrow().is_none() {
                *self.page.borrow_mut() = handed;
            }
        }
        self.focus.set(next);
        if next.is_covered() && !before.is_covered() {
            self.pointer.borrow_mut().cover();
        }
        if let Some(active) = changed {
            if active {
                self.im.focus_in();
            } else {
                release(&self.im);
            }
            self.send(ViewEvent::Focus(active));
        }
        if before.is_native() && !next.is_native() {
            self.disown();
            let page = self.page.borrow_mut().take();
            if !restore {
                return page;
            }
            self.give_page_focus(page);
        }
        None
    }

    /// Makes this view its toplevel's native focus owner and releases the view that was,
    /// without giving GTK focus back. Output: the widget the released view would have given
    /// it back to.
    fn supersede(self: &Rc<Self>) -> Option<WeakRef<gtk::Widget>> {
        let previous = self.owner.view.replace(Rc::downgrade(self)).upgrade();
        let previous = previous.filter(|previous| !Rc::ptr_eq(previous, self))?;
        let handed = previous.moving(|f| f.native(false), false);
        assert!(!previous.focus.get().is_native());
        assert!(Weak::ptr_eq(
            &self.owner.view.borrow(),
            &Rc::downgrade(self)
        ));
        handed
    }

    /// Stops being the toplevel's native focus owner, if this view is. Output: none.
    fn disown(self: &Rc<Self>) {
        let owning = Weak::ptr_eq(&self.owner.view.borrow(), &Rc::downgrade(self));
        if owning {
            *self.owner.view.borrow_mut() = Weak::new();
        }
    }

    /// Notes whether the view's rectangle shows inside the page now; going off screen
    /// releases native focus and gives GTK focus back to the page. Output: none.
    fn reshow(self: &Rc<Self>) {
        let rect = self.pointer.borrow().rect;
        let shown = rect
            .zip(page_bounds(&self.area, &self.toplevel))
            .is_some_and(|(rect, bounds)| rect.visible(bounds).is_some());
        self.moving(|f| f.shown(shown), true);
    }

    fn take_page_focus(&self) {
        let page = self
            .toplevel
            .focused_widget()
            .map(|widget| widget.downgrade());
        *self.page.borrow_mut() = page;
        self.toplevel.set_focus(None::<&gtk::Widget>);
        assert!(self.toplevel.focused_widget().is_none());
    }

    fn give_page_focus(&self, page: Option<WeakRef<gtk::Widget>>) {
        let Some(page) = page.and_then(|weak| weak.upgrade()) else {
            return;
        };
        let toplevel: &gtk::Widget = self.toplevel.upcast_ref();
        if page.toplevel().as_ref() == Some(toplevel) {
            page.grab_focus();
        }
    }

    /// Takes one key press while the view is active, ahead of the webview: a Control
    /// chord reads the key's Latin group on a non-Latin layout, a chord becomes its
    /// command, the input method gets the key next, and a key it does not take goes to
    /// the key map. Input: the event. Output: whether the key goes on to the page, which
    /// it does only while inactive.
    fn key_press(&self, event: &EventKey) -> Propagation {
        if !self.focus.get().active() {
            return Propagation::Proceed;
        }
        let state = event.state();
        let keyval = keys::control_latin(event.keyval(), state, || {
            self.entries(event.hardware_keycode())
        });
        if let Some(command) = chord(keyval, state) {
            self.send(command);
            return Propagation::Stop;
        }
        if self.im.filter_keypress(event) {
            return Propagation::Stop;
        }
        if let Some(input) = keys::key_input(keyval, state) {
            self.send(ViewEvent::Key(input));
        }
        Propagation::Stop
    }

    fn entries(&self, keycode: u16) -> Vec<keys::Entry> {
        let display = WidgetExt::display(&self.toplevel);
        gdk::Keymap::for_display(&display)
            .map(|keymap| {
                keymap
                    .entries_for_keycode(u32::from(keycode))
                    .into_iter()
                    .map(|(entry, keyval)| (entry.group(), entry.level(), Key::from(keyval)))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn key_release(&self, event: &EventKey) -> Propagation {
        if !self.focus.get().active() {
            return Propagation::Proceed;
        }
        self.im.filter_keypress(event);
        Propagation::Stop
    }

    /// Routes one pointer event the page widget got, ahead of the page's own handlers. A
    /// press the page gets releases native focus; a middle press reads PRIMARY, whose text
    /// lands only if the view is still alive, placed and uncovered when it comes back.
    /// Input: the event. Output: whether it goes on to the page.
    fn pointer_event(self: &Rc<Self>, event: &gdk::Event) -> Propagation {
        let kind = event.event_type();
        let pointer_kind = matches!(
            kind,
            EventType::ButtonPress
                | EventType::DoubleButtonPress
                | EventType::TripleButtonPress
                | EventType::ButtonRelease
                | EventType::MotionNotify
                | EventType::Scroll
        );
        if !pointer_kind {
            return Propagation::Proceed;
        }
        let Some(at) = self.toplevel_point(event) else {
            return Propagation::Proceed;
        };
        let covered = self.focus.get().is_covered();
        let state = event.state().unwrap_or(ModifierType::empty());
        let option = state.contains(ModifierType::MOD1_MASK);
        let button = event.button().unwrap_or(0);
        let route = {
            let mut pointer = self.pointer.borrow_mut();
            match kind {
                EventType::ButtonPress => {
                    pointer.press(button, at, event.time(), state, covered, self.double)
                }
                EventType::DoubleButtonPress | EventType::TripleButtonPress => pointer.repeat(),
                EventType::ButtonRelease => pointer.release(button, at, option),
                EventType::MotionNotify => pointer.motion(at, option),
                _ => {
                    let (dy, touchpad) = scroll_dy(event);
                    pointer.scroll(at, dy, touchpad, covered)
                }
            }
        };
        match route {
            Route::Page => {
                if kind == EventType::ButtonPress && self.focus.get().is_native() {
                    self.moving(|f| f.native(false), true);
                }
                Propagation::Proceed
            }
            Route::Primary => {
                let me = Rc::downgrade(self);
                self.clip.read(Board::Primary, move |text| {
                    let Some(me) = me.upgrade() else {
                        return;
                    };
                    let placed = me.pointer.borrow().is_placed();
                    if let Some(event) = primary_paste(me.focus.get(), placed, text) {
                        me.send(event);
                    }
                });
                Propagation::Stop
            }
            Route::View { focus, event } => {
                if focus {
                    self.moving(|f| f.native(true), true);
                }
                if let Some(event) = event {
                    self.send(event);
                }
                Propagation::Stop
            }
        }
    }

    fn toplevel_point(&self, event: &gdk::Event) -> Option<(f64, f64)> {
        let (root_x, root_y) = event.root_coords()?;
        let (_, x, y) = self.toplevel.window()?.origin();
        let at = (root_x - f64::from(x), root_y - f64::from(y));
        (at.0.is_finite() && at.1.is_finite()).then_some(at)
    }
}

/// The Linux side of one native terminal view's input. It takes the toplevel's keys
/// ahead of the webview while active, through its own input method context, and the
/// pointer events of its rectangle from the page widget's `event` signal, which GTK 3
/// emits before the page's own button, motion and scroll handlers. It hands each key,
/// commit, pointer event and focus change to the host's sink with the view's id. Native
/// focus takes GTK focus off the page, as the macOS view takes first responder, and gives
/// it back on release; the page taking GTK focus releases native focus.
pub struct View {
    inner: Rc<Inner>,
    handlers: Handlers,
    page_handlers: Handlers,
    commit: Option<SignalHandlerId>,
}

impl View {
    /// Wires a view to a realized toplevel and the page widget under its rectangle,
    /// unplaced and without native focus. Inputs: the toplevel, the page widget, the view
    /// id, the sink and the host's boards for the middle-click paste. Output: the view, or
    /// `native_terminal_surface_unavailable` when the toplevel has no GDK window or the
    /// page is not inside it.
    pub fn new(
        toplevel: &gtk::Window,
        page: &gtk::Widget,
        id: u64,
        sink: Sink,
        clip: Rc<Clip>,
    ) -> Result<View, &'static str> {
        assert!(id > 0);
        let gdk = toplevel
            .window()
            .ok_or("native_terminal_surface_unavailable")?;
        if page.toplevel().as_ref() != Some(toplevel.upcast_ref::<gtk::Widget>()) {
            return Err("native_terminal_surface_unavailable");
        }
        let im = gtk::IMMulticontext::new();
        im.set_client_window(Some(&gdk));
        let inner = Rc::new(Inner {
            id,
            sink,
            im,
            focus: Cell::new(Focus::new(toplevel.has_toplevel_focus())),
            toplevel: toplevel.clone(),
            area: page.clone(),
            owner: owner_of(toplevel),
            page: RefCell::new(None),
            pointer: RefCell::new(Pointer::new()),
            double: DoubleClick::from_settings(),
            clip,
        });
        let commit = {
            let me = Rc::clone(&inner);
            inner.im.connect_commit(move |_, text| {
                if let Some(event) = commit_event(me.focus.get(), text) {
                    me.send(event);
                }
            })
        };
        let me = || Rc::clone(&inner);
        let mut handlers = Handlers::new(toplevel);
        for id in [
            {
                let me = me();
                toplevel.connect_key_press_event(move |_, event| me.key_press(event))
            },
            {
                let me = me();
                toplevel.connect_key_release_event(move |_, event| me.key_release(event))
            },
            {
                let me = me();
                toplevel.connect_focus_in_event(move |_, _| {
                    me.moving(|f| f.toplevel(true), false);
                    Propagation::Proceed
                })
            },
            {
                let me = me();
                toplevel.connect_focus_out_event(move |_, _| {
                    me.moving(|f| f.toplevel(false), false);
                    Propagation::Proceed
                })
            },
            {
                let me = me();
                toplevel.connect_set_focus(move |_, widget| {
                    if widget.is_some() && me.focus.get().is_native() {
                        me.moving(|f| f.native(false), false);
                    }
                })
            },
        ] {
            handlers.push(id);
        }
        let mut page_handlers = Handlers::new(page);
        page_handlers.push({
            let me = me();
            page.connect_event(move |_, event| me.pointer_event(event))
        });
        page_handlers.push({
            let me = me();
            page.connect_size_allocate(move |_, _| me.reshow())
        });
        Ok(View {
            inner,
            handlers,
            page_handlers,
            commit: Some(commit),
        })
    }

    /// Places the view's rectangle in the toplevel's logical coordinates, or unplaces it. A
    /// view with no part of its rectangle inside the page, unplaced here or clipped away by
    /// the page's size-allocate, releases native focus and gives GTK focus back to the
    /// page; only a click or `focus` gives native focus back. Input: the rectangle.
    pub fn place(&self, rect: Option<Rect>) {
        self.inner.pointer.borrow_mut().place(rect);
        self.inner.reshow();
    }

    /// Gives or takes native focus, as a click in the host rectangle or the page asks;
    /// refused while covered. Input: on or off. Output: none.
    pub fn focus(&self, on: bool) {
        self.inner.moving(|f| f.native(on), true);
    }

    /// Covers or uncovers the view as the page reports an element over it; covering
    /// releases native focus and drops a held press, so keys and the pointer go back to
    /// the page. Input: covered or not. Output: none.
    pub fn cover(&self, covered: bool) {
        self.inner.moving(|f| f.cover(covered), true);
    }
}

impl Drop for View {
    /// Disconnects the toplevel, page and input method handlers, cancels any open preedit,
    /// releases the context and, if the view held native focus, stops owning it and gives
    /// GTK focus back to the page. Output: none.
    fn drop(&mut self) {
        let inner = &self.inner;
        let native = inner.focus.get().is_native();
        inner.disown();
        self.handlers.disconnect();
        self.page_handlers.disconnect();
        if let Some(commit) = self.commit.take() {
            inner.im.disconnect(commit);
        }
        if inner.focus.get().active() {
            release(&inner.im);
        }
        inner.im.set_client_window(None::<&gtk::gdk::Window>);
        if native {
            let page = inner.page.borrow_mut().take();
            inner.give_page_focus(page);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_term::event::MouseKind;
    use crate::native_term::linux::clip::Clip;
    use crate::native_term::linux::rows::{STEP_WAIT, checkpoint, pump_for, pump_until, row_dir};
    use crate::native_term::linux::surface::{self, Rect};
    use crate::native_term::pane::Named;
    use gtk::glib::translate::{IntoGlib, ToGlibPtr};
    use serde_json::json;
    use std::cell::RefCell;
    use std::time::Duration;
    use webkit2gtk::WebViewExt;

    /// The view id the display row's events carry.
    const ID: u64 = 7;

    /// A page whose textarea has focus and whose title mirrors the textarea, so the row
    /// reads what the page got.
    const PAGE: &str = "<!doctype html><title>v:</title><textarea id=\"t\" autofocus style=\"width:90vw;height:90vh\"></textarea><script>const t=document.getElementById('t');t.addEventListener('input',()=>{document.title='v:'+t.value});t.focus();</script>";

    thread_local! {
        static SEEN: RefCell<Vec<(u64, ViewEvent)>> = const { RefCell::new(Vec::new()) };
    }

    fn seen(id: u64, event: ViewEvent) {
        SEEN.with(|seen| seen.borrow_mut().push((id, event)));
    }

    fn seen_count() -> usize {
        SEEN.with(|seen| seen.borrow().len())
    }

    fn take_seen() -> Vec<ViewEvent> {
        SEEN.with(|seen| {
            let taken: Vec<(u64, ViewEvent)> = seen.borrow_mut().drain(..).collect();
            assert!(taken.iter().all(|(id, _)| *id == ID));
            taken.into_iter().map(|(_, event)| event).collect()
        })
    }

    fn text(bytes: &[u8]) -> ViewEvent {
        ViewEvent::Key(Input::Text(bytes.to_vec()))
    }

    #[test]
    fn the_terminal_chords_copy_paste_and_select_all() {
        let control_shift = ModifierType::CONTROL_MASK | ModifierType::SHIFT_MASK;
        assert_eq!(chord(key::C, control_shift), Some(ViewEvent::Copy));
        assert_eq!(chord(key::V, control_shift), Some(ViewEvent::Paste));
        assert_eq!(chord(key::A, control_shift), Some(ViewEvent::SelectAll));
        assert_eq!(chord(key::c, control_shift), Some(ViewEvent::Copy));
        let noise = ModifierType::LOCK_MASK
            | ModifierType::MOD2_MASK
            | ModifierType::MODIFIER_RESERVED_25_MASK
            | ModifierType::BUTTON1_MASK;
        assert_eq!(chord(key::V, control_shift | noise), Some(ViewEvent::Paste));
        let shift = ModifierType::SHIFT_MASK;
        assert_eq!(chord(key::Insert, shift), Some(ViewEvent::Paste));
        assert_eq!(chord(key::KP_Insert, shift), Some(ViewEvent::Paste));
    }

    #[test]
    fn a_chord_on_a_non_latin_layout_uses_the_keys_latin_group() {
        let ru_c = || {
            vec![
                (0, 0, key::c),
                (0, 1, key::C),
                (1, 0, key::Cyrillic_es),
                (1, 1, key::Cyrillic_ES),
            ]
        };
        let control_shift = ModifierType::CONTROL_MASK | ModifierType::SHIFT_MASK;
        let keyval = keys::control_latin(key::Cyrillic_ES, control_shift, ru_c);
        assert_eq!(chord(keyval, control_shift), Some(ViewEvent::Copy));
    }

    #[test]
    fn other_chords_stay_keys() {
        let control = ModifierType::CONTROL_MASK;
        let shift = ModifierType::SHIFT_MASK;
        assert_eq!(chord(key::c, control), None);
        assert_eq!(
            chord(key::C, control | shift | ModifierType::MOD1_MASK),
            None
        );
        assert_eq!(
            chord(key::C, control | shift | ModifierType::SUPER_MASK),
            None
        );
        assert_eq!(chord(key::X, control | shift), None);
        assert_eq!(chord(key::Insert, ModifierType::empty()), None);
        assert_eq!(chord(key::Insert, control), None);
        assert_eq!(chord(key::Insert, control | shift), None);
    }

    #[test]
    fn a_commit_is_one_text_input_within_its_bound() {
        assert_eq!(commit_input("a"), Some(Input::Text(b"a".to_vec())));
        assert_eq!(
            commit_input("é"),
            Some(Input::Text("é".as_bytes().to_vec()))
        );
        assert_eq!(commit_input(""), None);
        let most = "x".repeat(COMMIT_BYTES_MAX);
        assert!(commit_input(&most).is_some());
        assert_eq!(commit_input(&format!("{most}x")), None);
    }

    #[test]
    fn keys_go_to_the_view_only_with_native_and_toplevel_focus_and_no_cover() {
        let mut focus = Focus::new(true);
        assert_eq!(focus.shown(true), None);
        assert!(!focus.active());
        assert_eq!(focus.native(true), Some(true));
        assert_eq!(focus.native(true), None);
        assert_eq!(focus.toplevel(false), Some(false));
        assert_eq!(focus.native(false), None);
        assert_eq!(focus.toplevel(true), None);
        assert_eq!(focus.native(true), Some(true));
        assert!(focus.active());
    }

    #[test]
    fn a_cover_drops_native_focus_until_the_page_gives_it_back() {
        let mut focus = Focus::new(true);
        assert_eq!(focus.shown(true), None);
        assert_eq!(focus.native(true), Some(true));
        assert_eq!(focus.cover(true), Some(false));
        assert_eq!(focus.native(true), None);
        assert!(!focus.active());
        assert_eq!(focus.cover(false), None);
        assert!(!focus.active());
        assert_eq!(focus.native(true), Some(true));
    }

    #[test]
    fn a_commit_lands_only_while_the_view_is_active() {
        let mut focus = Focus::new(true);
        assert_eq!(focus.shown(true), None);
        assert_eq!(commit_event(focus, "a"), None);
        assert_eq!(focus.native(true), Some(true));
        assert_eq!(commit_event(focus, "a"), Some(text(b"a")));
        assert_eq!(commit_event(focus, ""), None);
        assert_eq!(focus.toplevel(false), Some(false));
        assert_eq!(commit_event(focus, "a"), None);
        assert_eq!(focus.toplevel(true), Some(true));
        assert_eq!(focus.cover(true), Some(false));
        assert_eq!(commit_event(focus, "a"), None);
        assert_eq!(focus.cover(false), None);
        assert_eq!(commit_event(focus, "a"), None);
    }

    #[test]
    fn a_view_off_screen_refuses_native_focus_and_going_off_screen_drops_it() {
        let mut focus = Focus::new(true);
        assert_eq!(
            focus.native(true),
            None,
            "a view not on screen refuses native focus"
        );
        assert_eq!(focus.shown(true), None);
        assert_eq!(focus.native(true), Some(true));
        assert_eq!(focus.shown(false), Some(false));
        assert!(!focus.is_native(), "going off screen drops native focus");
        assert_eq!(focus.native(true), None);
        assert_eq!(focus.shown(true), None);
        assert!(
            !focus.is_native(),
            "coming back on screen gives no native focus back"
        );
        assert_eq!(focus.native(true), Some(true));
    }

    const DOUBLE: DoubleClick = DoubleClick {
        time_ms: 400,
        distance: 5.0,
    };

    const RECT: Rect = Rect {
        x: 100,
        y: 120,
        w: 400,
        h: 240,
    };

    fn placed() -> Pointer {
        let mut pointer = Pointer::new();
        pointer.place(Some(RECT));
        pointer
    }

    fn mouse(kind: MouseKind, point: (f64, f64), clicks: u8) -> Option<ViewEvent> {
        Some(ViewEvent::Mouse {
            kind,
            point,
            option: false,
            clicks,
        })
    }

    fn view(focus: bool, event: Option<ViewEvent>) -> Route {
        Route::View { focus, event }
    }

    fn left(pointer: &mut Pointer, at: (f64, f64), time: u32) -> Route {
        let route = pointer.press(1, at, time, ModifierType::empty(), false, DOUBLE);
        assert!(matches!(pointer.release(1, at, false), Route::View { .. }));
        route
    }

    #[test]
    fn a_press_inside_the_uncovered_rect_takes_focus_and_counts_clicks() {
        let mut pointer = placed();
        assert_eq!(
            pointer.press(
                1,
                (110.0, 130.0),
                1_000,
                ModifierType::empty(),
                false,
                DOUBLE
            ),
            view(true, mouse(MouseKind::Press, (10.0, 10.0), 1))
        );
        assert_eq!(
            pointer.release(1, (110.0, 130.0), false),
            view(false, mouse(MouseKind::Release, (10.0, 10.0), 1))
        );
        assert_eq!(
            left(&mut pointer, (112.0, 131.0), 1_200),
            view(true, mouse(MouseKind::Press, (12.0, 11.0), 2))
        );
        assert_eq!(
            left(&mut pointer, (112.0, 131.0), 1_300),
            view(true, mouse(MouseKind::Press, (12.0, 11.0), 3))
        );
        assert_eq!(
            left(&mut pointer, (112.0, 131.0), 1_400),
            view(true, mouse(MouseKind::Press, (12.0, 11.0), 3))
        );
        assert_eq!(
            pointer.press(
                1,
                (499.0, 359.0),
                9_000,
                ModifierType::MOD1_MASK,
                false,
                DOUBLE
            ),
            view(
                true,
                Some(ViewEvent::Mouse {
                    kind: MouseKind::Press,
                    point: (399.0, 239.0),
                    option: true,
                    clicks: 1,
                })
            )
        );
    }

    #[test]
    fn a_press_outside_the_rect_on_a_covered_view_or_before_placing_goes_to_the_page() {
        let mut unplaced = Pointer::new();
        assert_eq!(
            unplaced.press(1, (110.0, 130.0), 0, ModifierType::empty(), false, DOUBLE),
            Route::Page
        );
        assert_eq!(
            unplaced.scroll((110.0, 130.0), 1.0, false, false),
            Route::Page
        );
        let mut pointer = placed();
        for outside in [
            (99.0, 130.0),
            (500.0, 130.0),
            (110.0, 119.0),
            (110.0, 360.0),
        ] {
            assert_eq!(
                pointer.press(1, outside, 0, ModifierType::empty(), false, DOUBLE),
                Route::Page
            );
            assert_eq!(pointer.release(1, outside, false), Route::Page);
            assert_eq!(pointer.scroll(outside, 1.0, false, false), Route::Page);
        }
        assert_eq!(pointer.motion((110.0, 130.0), false), Route::Page);
        for button in [1, 2, 3] {
            assert_eq!(
                pointer.press(
                    button,
                    (110.0, 130.0),
                    0,
                    ModifierType::empty(),
                    true,
                    DOUBLE
                ),
                Route::Page
            );
        }
        assert_eq!(
            pointer.scroll((110.0, 130.0), 1.0, false, true),
            Route::Page
        );
        pointer.place(None);
        assert_eq!(
            pointer.press(1, (110.0, 130.0), 0, ModifierType::empty(), false, DOUBLE),
            Route::Page
        );
    }

    #[test]
    fn a_press_while_another_button_is_down_goes_to_the_page() {
        let mut pointer = placed();
        let inside = (110.0, 130.0);
        let left_down = ModifierType::BUTTON1_MASK;
        assert_eq!(
            pointer.press(3, inside, 0, left_down, false, DOUBLE),
            Route::Page,
            "a press while the page holds the left button stays the page's"
        );
        assert_eq!(
            pointer.release(1, inside, false),
            Route::Page,
            "so the page gets its left release"
        );
        assert_eq!(pointer.release(3, inside, false), Route::Page);
        assert_eq!(
            pointer.press(1, inside, 0, left_down, false, DOUBLE),
            view(true, mouse(MouseKind::Press, (10.0, 10.0), 1)),
            "a press's own button in its state is no other button"
        );
        assert_eq!(
            pointer.release(1, inside, false),
            view(false, mouse(MouseKind::Release, (10.0, 10.0), 1))
        );
        let mut first = placed();
        let mut second = placed();
        assert!(matches!(
            first.press(1, inside, 0, ModifierType::empty(), false, DOUBLE),
            Route::View { .. }
        ));
        assert_eq!(
            second.press(3, inside, 0, left_down, false, DOUBLE),
            Route::Page,
            "a second view starts no press while the first holds one"
        );
        assert_eq!(second.release(1, inside, false), Route::Page);
        assert_eq!(
            first.release(1, inside, false),
            view(false, mouse(MouseKind::Release, (10.0, 10.0), 1)),
            "the first view gets its release"
        );
    }

    #[test]
    fn a_left_drag_follows_the_pointer_outside_the_rect_until_release() {
        let mut pointer = placed();
        pointer.press(1, (110.0, 130.0), 0, ModifierType::empty(), false, DOUBLE);
        assert_eq!(
            pointer.motion((50.0, 400.0), false),
            view(false, mouse(MouseKind::Drag, (-50.0, 280.0), 1))
        );
        assert_eq!(
            pointer.release(1, (50.0, 400.0), false),
            view(false, mouse(MouseKind::Release, (-50.0, 280.0), 1))
        );
        assert_eq!(pointer.motion((110.0, 130.0), false), Route::Page);
    }

    #[test]
    fn the_right_button_opens_the_menu_and_other_buttons_are_held_without_events() {
        let mut pointer = placed();
        assert_eq!(
            pointer.press(3, (110.0, 130.0), 0, ModifierType::empty(), false, DOUBLE),
            view(
                false,
                Some(ViewEvent::Menu {
                    point: (10.0, 10.0)
                })
            )
        );
        assert_eq!(pointer.motion((120.0, 140.0), false), view(false, None));
        assert_eq!(
            pointer.press(1, (110.0, 130.0), 0, ModifierType::empty(), false, DOUBLE),
            view(false, None)
        );
        assert_eq!(pointer.release(1, (110.0, 130.0), false), view(false, None));
        assert_eq!(pointer.release(3, (110.0, 130.0), false), view(false, None));
        assert_eq!(pointer.motion((110.0, 130.0), false), Route::Page);
        assert_eq!(
            pointer.press(8, (110.0, 130.0), 0, ModifierType::empty(), false, DOUBLE),
            view(false, None)
        );
        assert_eq!(pointer.release(8, (110.0, 130.0), false), view(false, None));
    }

    #[test]
    fn a_middle_press_inside_reads_primary_and_is_held_until_release() {
        let mut pointer = placed();
        assert_eq!(
            pointer.press(2, (110.0, 130.0), 0, ModifierType::empty(), false, DOUBLE),
            Route::Primary
        );
        assert_eq!(pointer.motion((120.0, 140.0), false), view(false, None));
        assert_eq!(pointer.release(2, (120.0, 140.0), false), view(false, None));
        assert_eq!(
            pointer.press(2, (99.0, 130.0), 0, ModifierType::empty(), false, DOUBLE),
            Route::Page
        );
        assert_eq!(
            pointer.press(2, (110.0, 130.0), 0, ModifierType::empty(), true, DOUBLE),
            Route::Page
        );
    }

    #[test]
    fn a_primary_paste_lands_only_on_a_placed_uncovered_view() {
        let text = || Some("p".to_owned());
        let paste = Some(ViewEvent::Key(Input::Paste {
            text: "p".to_owned(),
            primary: true,
        }));
        let mut focus = Focus::new(true);
        assert_eq!(focus.shown(true), None);
        assert_eq!(primary_paste(focus, true, text()), paste);
        assert_eq!(primary_paste(focus, false, text()), None);
        assert_eq!(primary_paste(focus, true, None), None);
        assert_eq!(focus.cover(true), None);
        assert_eq!(primary_paste(focus, true, text()), None);
    }

    #[test]
    fn a_cover_during_a_press_leaves_the_rest_of_it_to_the_page() {
        let mut pointer = placed();
        pointer.press(1, (110.0, 130.0), 0, ModifierType::empty(), false, DOUBLE);
        pointer.cover();
        assert_eq!(pointer.motion((120.0, 140.0), false), Route::Page);
        assert_eq!(pointer.release(1, (120.0, 140.0), false), Route::Page);
    }

    /// A GDK scroll event of a direction and deltas, built as GDK builds one; no display
    /// is needed. Inputs: the direction and the deltas. Output: the event.
    fn scroll_event(direction: gdk::ffi::GdkScrollDirection, dx: f64, dy: f64) -> gdk::Event {
        unsafe {
            let event = gdk::ffi::gdk_event_new(gdk::ffi::GDK_SCROLL);
            assert!(!event.is_null());
            (*event).scroll.direction = direction;
            (*event).scroll.delta_x = dx;
            (*event).scroll.delta_y = dy;
            gtk::glib::translate::from_glib_full(event)
        }
    }

    /// A real wheel or touchpad sends smooth scrolls, whose GDK direction reads as none;
    /// the view takes their deltas. Only the farm's XTest wheel sends discrete ones.
    #[test]
    fn a_smooth_scroll_gives_its_vertical_delta_and_a_discrete_one_a_line() {
        assert_eq!(
            scroll_dy(&scroll_event(gdk::ffi::GDK_SCROLL_SMOOTH, 0.0, 1.5)),
            (1.5, false)
        );
        assert_eq!(
            scroll_dy(&scroll_event(gdk::ffi::GDK_SCROLL_SMOOTH, 0.0, -0.25)),
            (-0.25, false)
        );
        assert_eq!(
            scroll_dy(&scroll_event(gdk::ffi::GDK_SCROLL_SMOOTH, 2.0, 0.0)),
            (0.0, false)
        );
        assert_eq!(
            scroll_dy(&scroll_event(gdk::ffi::GDK_SCROLL_DOWN, 0.0, 0.0)),
            (1.0, false)
        );
        assert_eq!(
            scroll_dy(&scroll_event(gdk::ffi::GDK_SCROLL_UP, 0.0, 0.0)),
            (-1.0, false)
        );
    }

    #[test]
    fn a_wheel_inside_is_lines_for_a_mouse_and_points_for_a_touchpad() {
        let pointer = placed();
        let wheel = |delta, precise| {
            view(
                false,
                Some(ViewEvent::Wheel {
                    point: (10.0, 10.0),
                    delta,
                    precise,
                }),
            )
        };
        assert_eq!(
            pointer.scroll((110.0, 130.0), 1.0, false, false),
            wheel(-1.0, false)
        );
        assert_eq!(
            pointer.scroll((110.0, 130.0), -2.0, false, false),
            wheel(2.0, false)
        );
        assert_eq!(
            pointer.scroll((110.0, 130.0), 0.25, true, false),
            wheel(-0.25 * TOUCHPAD_POINTS_PER_UNIT, true)
        );
        assert_eq!(
            pointer.scroll((110.0, 130.0), f64::NAN, false, false),
            view(false, None)
        );
    }

    #[test]
    fn clicks_reset_after_the_double_click_time_or_distance() {
        let mut pointer = placed();
        let clicks = |route: Route| match route {
            Route::View {
                event: Some(ViewEvent::Mouse { clicks, .. }),
                ..
            } => clicks,
            other => panic!("a press expected, got {other:?}"),
        };
        assert_eq!(clicks(left(&mut pointer, (110.0, 130.0), 1_000)), 1);
        assert_eq!(clicks(left(&mut pointer, (110.0, 130.0), 1_401)), 1);
        assert_eq!(clicks(left(&mut pointer, (116.0, 130.0), 1_500)), 1);
        assert_eq!(clicks(left(&mut pointer, (116.0, 134.0), 1_600)), 2);
        assert_eq!(
            clicks(left(&mut pointer, (116.0, 134.0), u32::MAX - 100)),
            1
        );
        assert_eq!(clicks(left(&mut pointer, (116.0, 134.0), 50)), 2);
        pointer.press(3, (116.0, 134.0), 100, ModifierType::empty(), false, DOUBLE);
        pointer.release(3, (116.0, 134.0), false);
        assert_eq!(clicks(left(&mut pointer, (116.0, 134.0), 150)), 1);
    }

    /// The ids of the two-view rows' views: A, wired to the page first, and B.
    const A: u64 = 11;
    const B: u64 = 12;

    const RECT_A: Rect = Rect {
        x: 20,
        y: 20,
        w: 200,
        h: 100,
    };

    const RECT_B: Rect = Rect {
        x: 300,
        y: 20,
        w: 200,
        h: 100,
    };

    /// A toplevel whose page widget sits at its top left in a fixed container, and two
    /// placed views on the page, A wired first, then B, which holds native focus in a
    /// toplevel both treat as focused. Output: the toplevel, the page, its GDK window and
    /// the views.
    fn two_views() -> (gtk::Window, gtk::DrawingArea, gdk::Window, View, View) {
        gtk::init().expect("a display");
        let window = gtk::Window::new(gtk::WindowType::Toplevel);
        window.set_title("native-row");
        window.set_default_size(640, 480);
        let fixed = gtk::Fixed::new();
        let page = gtk::DrawingArea::new();
        page.set_size_request(600, 400);
        fixed.put(&page, 0, 0);
        window.add(&fixed);
        window.show_all();
        pump_until("the toplevel mapped", || {
            window.window().is_some_and(|gdk| gdk.is_viewable())
        });
        pump_for(Duration::from_millis(300));
        let gdk = window.window().expect("a GDK window");
        let view = |id| {
            View::new(
                &window,
                page.upcast_ref(),
                id,
                seen,
                Rc::new(Clip::private()),
            )
            .expect("a view")
        };
        let a = view(A);
        let b = view(B);
        a.place(Some(RECT_A));
        b.place(Some(RECT_B));
        for one in [&a, &b] {
            one.inner.moving(|f| f.toplevel(true), false);
        }
        b.focus(true);
        assert!(b.inner.focus.get().active(), "B takes keys");
        (window, page, gdk, a, b)
    }

    /// A key press of a key on a window, built as GDK builds one. Inputs: the window and
    /// the key. Output: the event.
    fn key_event(window: &gdk::Window, keyval: Key) -> EventKey {
        let event: gdk::Event = unsafe {
            let event = gdk::ffi::gdk_event_new(gdk::ffi::GDK_KEY_PRESS);
            assert!(!event.is_null());
            (*event).key.window = window.to_glib_full();
            (*event).key.keyval = keyval.into_glib();
            (*event).key.time = 1;
            gtk::glib::translate::from_glib_full(event)
        };
        event.downcast::<EventKey>().expect("a key event")
    }

    /// A left button event at a point of a toplevel, built as GDK builds one. Inputs: the
    /// event type, the toplevel's GDK window and the point. Output: the event.
    fn left_event(
        kind: gdk::ffi::GdkEventType,
        window: &gdk::Window,
        at: (f64, f64),
    ) -> gdk::Event {
        let (_, x, y) = window.origin();
        unsafe {
            let event = gdk::ffi::gdk_event_new(kind);
            assert!(!event.is_null());
            (*event).button.window = window.to_glib_full();
            (*event).button.time = 1;
            (*event).button.x = at.0;
            (*event).button.y = at.1;
            (*event).button.x_root = f64::from(x) + at.0;
            (*event).button.y_root = f64::from(y) + at.1;
            (*event).button.button = 1;
            gtk::glib::translate::from_glib_full(event)
        }
    }

    /// Resizes the page widget and runs GTK's layout. Inputs: the page and its size.
    fn resize_page(page: &gtk::DrawingArea, size: (i32, i32)) {
        page.set_size_request(size.0, size.1);
        pump_until("the page's new allocation", || {
            (page.allocated_width(), page.allocated_height()) == size
        });
        pump_for(Duration::from_millis(100));
    }

    fn assert_no_keys() {
        let seen: Vec<(u64, ViewEvent)> = SEEN.with(|seen| seen.borrow_mut().drain(..).collect());
        assert!(
            seen.iter()
                .all(|(_, event)| !matches!(event, ViewEvent::Key(_))),
            "no key reached a view: {seen:?}"
        );
    }

    fn close(window: gtk::Window, a: View, b: View) {
        drop(b);
        drop(a);
        unsafe {
            window.destroy();
        }
    }

    /// A view the host unplaces while it holds native focus sends its keys on to the page
    /// and gets no native focus back when placed again.
    #[test]
    #[ignore = "needs a display: the farm runs it under Xvfb and sway"]
    fn an_unplaced_view_sends_its_keys_on_to_the_page() {
        let (window, _page, gdk, a, b) = two_views();
        let x = key_event(&gdk, key::x);
        assert!(b.inner.focus.get().active());
        b.place(None);
        assert_eq!(
            b.inner.key_press(&x),
            Propagation::Proceed,
            "an unplaced view's keys go on to the page"
        );
        b.place(Some(RECT_B));
        assert!(
            !b.inner.focus.get().is_native(),
            "placing again gives no native focus back"
        );
        b.focus(true);
        assert!(b.inner.focus.get().active());
        assert_no_keys();
        close(window, a, b);
    }

    /// A view whose rectangle the page shrinks away from, with no host call, sends its keys
    /// on to the page and gets no native focus back when the page grows again.
    #[test]
    #[ignore = "needs a display: the farm runs it under Xvfb and sway"]
    fn a_view_clipped_out_of_the_page_sends_its_keys_on_to_the_page() {
        let (window, page, gdk, a, b) = two_views();
        let x = key_event(&gdk, key::x);
        assert!(b.inner.focus.get().active());
        resize_page(&page, (250, 400));
        assert_eq!(
            b.inner.key_press(&x),
            Propagation::Proceed,
            "a view with no part inside the page takes no keys"
        );
        resize_page(&page, (600, 400));
        assert!(
            !b.inner.focus.get().is_native(),
            "showing again gives no native focus back"
        );
        b.focus(true);
        assert!(b.inner.focus.get().active());
        assert_no_keys();
        close(window, a, b);
    }

    /// A press in view A, wired to the page first, takes native focus from view B, whose
    /// handler never sees the press, so a cover over A leaves the keys to neither view.
    #[test]
    #[ignore = "needs a display: the farm runs it under Xvfb and sway"]
    fn a_press_in_one_view_takes_native_focus_from_the_other() {
        let (window, page, gdk, a, b) = two_views();
        let x = key_event(&gdk, key::x);
        let at = (60.0, 60.0);
        assert!(
            page.event(&left_event(gdk::ffi::GDK_BUTTON_PRESS, &gdk, at)),
            "A takes a press in its rectangle"
        );
        assert!(page.event(&left_event(gdk::ffi::GDK_BUTTON_RELEASE, &gdk, at)));
        assert!(
            a.inner.focus.get().active(),
            "the press gives A native focus"
        );
        assert!(
            !b.inner.focus.get().is_native(),
            "A taking native focus releases B"
        );
        a.cover(true);
        assert_eq!(a.inner.key_press(&x), Propagation::Proceed);
        assert_eq!(
            b.inner.key_press(&x),
            Propagation::Proceed,
            "with A covered, keys go to neither view"
        );
        assert_no_keys();
        close(window, a, b);
    }

    fn page_text(page: &webkit2gtk::WebView) -> String {
        page.title()
            .map(|title| title.to_string())
            .unwrap_or_default()
    }

    fn step(name: &str) {
        checkpoint(name, &json!({ "step": name }));
    }

    /// A WebKit page's textarea has focus and takes typed and composed text; then the
    /// view takes native focus and gets each key and each input method commit once, while
    /// the page gets none; a cover sends keys back to the page, and a preedit open at a
    /// cover or a close never commits into the view. Spike run 3 left open whether the
    /// view composes on sway after the page composed; this is that red row.
    #[test]
    #[ignore = "needs a display, IBus and the farm's typing: the farm runs it under Xvfb and sway"]
    fn a_focused_view_takes_keys_and_commits_after_the_page_had_them() {
        assert!(
            row_dir().is_some(),
            "NATIVE_ROW_DIR: the farm types the keys"
        );
        gtk::init().expect("a display");
        let window = gtk::Window::new(gtk::WindowType::Toplevel);
        window.set_title("native-row");
        window.set_default_size(640, 480);
        let page = webkit2gtk::WebView::new();
        window.add(&page);
        window.show_all();
        page.load_html(PAGE, None);
        pump_until("page load", || {
            !page.is_loading() && page_text(&page) == "v:"
        });
        page.grab_focus();
        pump_for(Duration::from_millis(300));
        let view = View::new(
            &window,
            page.upcast_ref(),
            ID,
            seen,
            Rc::new(Clip::desktop()),
        )
        .expect("a view");
        view.place(Some(Rect {
            x: 20,
            y: 20,
            w: 200,
            h: 100,
        }));

        step("page");
        pump_until("page typing", || page_text(&page) == "v:xy");
        assert_eq!(take_seen(), Vec::new());

        assert!(page.is_focus());
        view.focus(true);
        assert_eq!(take_seen(), vec![ViewEvent::Focus(true)]);
        assert!(
            !page.is_focus(),
            "native focus takes GTK focus off the page"
        );
        step("native");
        pump_until("view keys", || seen_count() >= 4);
        pump_for(Duration::from_millis(200));
        assert_eq!(
            take_seen(),
            vec![
                text(b"a"),
                text(b"b"),
                text(b"\r"),
                ViewEvent::Key(Input::Key {
                    key: Named::Up,
                    mods: 0
                }),
            ]
        );
        assert_eq!(page_text(&page), "v:xy");

        page.grab_focus();
        assert_eq!(
            take_seen(),
            vec![ViewEvent::Focus(false)],
            "the page taking GTK focus releases native focus"
        );
        step("page-ime");
        pump_until("page composition", || page_text(&page) == "v:xyé");

        view.focus(true);
        assert_eq!(take_seen(), vec![ViewEvent::Focus(true)]);
        assert!(!page.is_focus());
        step("native-ime");
        pump_until("view composition", || seen_count() >= 1);
        pump_for(Duration::from_millis(300));
        assert_eq!(take_seen(), vec![text("á".as_bytes())]);
        assert_eq!(page_text(&page), "v:xyé");

        step("native-preedit");
        pump_for(Duration::from_millis(300));
        assert_eq!(take_seen(), Vec::new(), "an open preedit commits nothing");
        view.cover(true);
        assert_eq!(take_seen(), vec![ViewEvent::Focus(false)]);
        pump_for(Duration::from_millis(500));
        assert_eq!(
            take_seen(),
            Vec::new(),
            "no commit lands in the view after a cover"
        );
        assert!(page.is_focus(), "a cover gives GTK focus back to the page");
        view.focus(true);
        assert_eq!(
            take_seen(),
            Vec::new(),
            "a covered view refuses native focus"
        );
        step("covered");
        pump_until("covered typing", || page_text(&page) == "v:xyéz");
        assert_eq!(take_seen(), Vec::new());

        view.cover(false);
        view.focus(true);
        assert_eq!(take_seen(), vec![ViewEvent::Focus(true)]);
        step("close-preedit");
        pump_for(Duration::from_millis(300));
        assert_eq!(take_seen(), Vec::new(), "an open preedit commits nothing");
        drop(view);
        pump_for(Duration::from_millis(500));
        assert_eq!(
            take_seen(),
            Vec::new(),
            "no commit lands in the view after a close"
        );
        assert!(page.is_focus(), "a close gives GTK focus back to the page");
        step("closed");
        pump_until("typing after close", || page_text(&page) == "v:xyézy");
        assert_eq!(take_seen(), Vec::new());
    }

    /// A page with an overlay over the host rectangle; the title counts presses on the
    /// overlay, presses elsewhere on the page and wheel events anywhere on the page.
    const POINTER_PAGE: &str = "<!doctype html><title>o:0 p:0 w:0</title><style>html,body{margin:0;height:200%}#o{position:absolute;left:100px;top:120px;width:400px;height:240px;background:#fff}</style><div id=\"o\"></div><script>let o=0,p=0,w=0;const s=()=>{document.title=`o:${o} p:${p} w:${w}`};document.getElementById('o').addEventListener('mousedown',e=>{o++;e.stopPropagation();s()});document.addEventListener('mousedown',()=>{p++;s()});document.addEventListener('wheel',()=>{w++;s()},{passive:true});</script>";

    fn counts(page: &webkit2gtk::WebView) -> [u32; 3] {
        let title = page_text(page);
        let mut out = [u32::MAX; 3];
        for (slot, part) in out.iter_mut().zip(title.split(' ')) {
            *slot = part
                .split_once(':')
                .and_then(|(_, n)| n.parse().ok())
                .unwrap_or(u32::MAX);
        }
        out
    }

    fn near(point: (f64, f64), want: (f64, f64)) -> bool {
        (point.0 - want.0).abs() <= 1.0 && (point.1 - want.1).abs() <= 1.0
    }

    fn pointer_step(name: &str, window: &gtk::Window, rect: Rect) {
        let gdk = window.window().expect("a realized toplevel");
        let (_, wx, wy) = gdk.origin();
        let click = [wx + rect.x + rect.w / 2, wy + rect.y + rect.h / 2];
        let outside = [wx + rect.x - 50, wy + rect.y + rect.h / 2];
        checkpoint(
            name,
            &json!({ "step": name, "click": click, "outside": outside }),
        );
    }

    /// A page overlay drawn over the host rectangle while the view is covered keeps its
    /// click and the wheel, and the view gets nothing; uncovered, a click and a wheel in
    /// the rectangle are the view's and the page gets neither; a click on the page
    /// outside the rectangle is the page's and releases native focus.
    #[test]
    #[ignore = "needs a display and the farm's pointer: the farm runs it under Xvfb and sway"]
    fn a_covered_view_leaves_clicks_and_wheel_to_the_page_overlay() {
        assert!(
            row_dir().is_some(),
            "NATIVE_ROW_DIR: the farm moves the pointer"
        );
        gtk::init().expect("a display");
        let window = gtk::Window::new(gtk::WindowType::Toplevel);
        window.set_title("native-row");
        window.set_decorated(false);
        window.set_default_size(640, 480);
        let page = webkit2gtk::WebView::new();
        window.add(&page);
        window.show_all();
        page.load_html(POINTER_PAGE, None);
        pump_until("page load", || {
            !page.is_loading() && counts(&page) == [0, 0, 0]
        });
        page.grab_focus();
        pump_for(Duration::from_millis(300));
        let offset = page
            .translate_coordinates(&window, 0, 0)
            .expect("the page inside the toplevel");
        let rect = surface::rect((100.0, 120.0, 400.0, 240.0), offset).expect("a host rect");
        let view = View::new(
            &window,
            page.upcast_ref(),
            ID,
            seen,
            Rc::new(Clip::desktop()),
        )
        .expect("a view");
        view.place(Some(rect));
        view.cover(true);
        assert_eq!(take_seen(), Vec::new());

        pointer_step("covered", &window, rect);
        pump_until("overlay click and wheel", || {
            let [o, _, w] = counts(&page);
            o == 1 && w >= 1 && w != u32::MAX
        });
        pump_for(Duration::from_millis(300));
        assert_eq!(
            take_seen(),
            Vec::new(),
            "a covered view takes no pointer event"
        );
        let [o, p, w] = counts(&page);
        assert_eq!((o, p), (1, 0));

        view.cover(false);
        assert_eq!(take_seen(), Vec::new());
        pump_until("toplevel focus", || window.has_toplevel_focus());
        pointer_step("uncovered", &window, rect);
        let waited = std::time::Instant::now();
        while waited.elapsed() < STEP_WAIT
            && !SEEN.with(|seen| {
                seen.borrow()
                    .iter()
                    .any(|(_, event)| matches!(event, ViewEvent::Wheel { .. }))
            })
        {
            pump_for(Duration::from_millis(5));
        }
        pump_for(Duration::from_millis(300));
        let got = take_seen();
        let center = (200.0, 120.0);
        assert!(
            got.len() >= 4,
            "the view got {got:?}; the page counts are {:?}",
            counts(&page)
        );
        assert_eq!(got[0], ViewEvent::Focus(true), "{got:?}");
        assert!(
            matches!(got[1], ViewEvent::Mouse { kind: MouseKind::Press, point, option: false, clicks: 1 } if near(point, center)),
            "{got:?}"
        );
        assert!(
            matches!(got[2], ViewEvent::Mouse { kind: MouseKind::Release, point, option: false, clicks: 1 } if near(point, center)),
            "{got:?}"
        );
        assert!(
            got[3..].iter().all(|event| matches!(event, ViewEvent::Wheel { point, delta, precise: false } if near(*point, center) && *delta < 0.0)),
            "{got:?}"
        );
        assert_eq!(
            counts(&page),
            [o, p, w],
            "the page gets nothing the view took"
        );

        pointer_step("outside", &window, rect);
        pump_until("page click", || counts(&page)[1] == p + 1);
        pump_for(Duration::from_millis(300));
        assert_eq!(
            take_seen(),
            vec![ViewEvent::Focus(false)],
            "a page click releases native focus"
        );
        drop(view);
    }

    /// A middle click inside the rectangle reads PRIMARY, which the farm set, and hands
    /// the text to the view's host as the same paste input Cmd-V makes, so the pane's paste
    /// checks and bracketed paste apply; the page gets no press. Covered, a middle click
    /// is the page's and pastes nothing into the view.
    #[test]
    #[ignore = "needs a display, the farm's pointer and its clipboard tools: the farm runs it under Xvfb and sway"]
    fn a_middle_click_pastes_primary_through_the_paste_path() {
        assert!(
            row_dir().is_some(),
            "NATIVE_ROW_DIR: the farm sets PRIMARY and clicks"
        );
        gtk::init().expect("a display");
        let window = gtk::Window::new(gtk::WindowType::Toplevel);
        window.set_title("native-row");
        window.set_decorated(false);
        window.set_default_size(640, 480);
        let page = webkit2gtk::WebView::new();
        window.add(&page);
        window.show_all();
        page.load_html(POINTER_PAGE, None);
        pump_until("page load", || {
            !page.is_loading() && counts(&page) == [0, 0, 0]
        });
        page.grab_focus();
        pump_for(Duration::from_millis(300));
        let offset = page
            .translate_coordinates(&window, 0, 0)
            .expect("the page inside the toplevel");
        let rect = surface::rect((100.0, 120.0, 400.0, 240.0), offset).expect("a host rect");
        let view = View::new(
            &window,
            page.upcast_ref(),
            ID,
            seen,
            Rc::new(Clip::desktop()),
        )
        .expect("a view");
        view.place(Some(rect));

        pointer_step("primary", &window, rect);
        pump_until("the primary paste", || seen_count() >= 1);
        pump_for(Duration::from_millis(300));
        assert_eq!(
            take_seen(),
            vec![ViewEvent::Key(Input::Paste {
                text: "farm-primary-3".to_owned(),
                primary: true,
            })]
        );
        assert_eq!(counts(&page), [0, 0, 0], "the page gets no press");

        view.cover(true);
        assert_eq!(take_seen(), Vec::new());
        pointer_step("covered-primary", &window, rect);
        pump_until("the overlay press", || counts(&page)[0] == 1);
        pump_for(Duration::from_millis(500));
        assert_eq!(take_seen(), Vec::new(), "a covered view pastes nothing");
        drop(view);
    }
}
