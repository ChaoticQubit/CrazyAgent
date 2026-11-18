use windows_sys::Win32::Foundation::HWND;

use super::super::event::Sink;
use super::super::screen::Frame;
use super::super::style::Style;

/// One terminal view's native child window over the webview on Windows.
pub struct View {
    hwnd: HWND,
}

impl View {
    /// Makes a hidden child window over the webview for one terminal view and hands it to
    /// the render thread. Inputs: the parent window, the webview window in it (or null), the
    /// view id, the host's event function and the style. Output: the view, or a plain code.
    pub fn new(
        parent: HWND,
        webview: HWND,
        id: u64,
        sink: Sink,
        style: &Style,
    ) -> Result<View, &'static str> {
        let _ = (parent, webview, id, sink, style);
        unimplemented!("H1 red")
    }

    /// The view's own window, for the host's focus and hit tests.
    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// Places the view over the page's terminal box and shows it, or hides it for none.
    /// Input: the box in CSS pixels. Output: ok, or `native_terminal_rect_invalid`.
    pub fn place(&self, page: Option<(f64, f64, f64, f64)>) -> Result<(), &'static str> {
        let _ = page;
        unimplemented!("H1 red")
    }

    /// Hides the view while a page popover, menu or dialog covers its box, and shows it
    /// again after. Input: whether it is covered.
    pub fn cover(&self, covered: bool) {
        let _ = covered;
        unimplemented!("H1 red")
    }

    /// Gives the view keyboard focus.
    pub fn focus(&self) {
        unimplemented!("H1 red")
    }

    /// Hands the render thread the view's newest screen. Input: the frame. Output: whether
    /// a live view took it.
    pub fn feed(&self, frame: Frame) -> bool {
        let _ = frame;
        unimplemented!("H1 red")
    }
}

impl Drop for View {
    /// Takes the view from the render thread, then destroys its window.
    fn drop(&mut self) {
        let _ = self.hwnd;
    }
}
