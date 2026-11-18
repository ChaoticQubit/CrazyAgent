use super::HookInput;
use super::view::TermView;
use super::view_text::range_or_none;
use objc2::msg_send;
use objc2::rc::Retained;
use objc2_foundation::{NSInteger, NSNotFound, NSRange, NSString};

/// What the hook's accessibility read saw through the view's own selectors: whether AppKit
/// gave the view an input context (it declares NSTextInputClient), its role, value,
/// insertion line, selected range and marked range.
#[derive(serde::Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AccessReport {
    pub input_context: bool,
    pub role: String,
    pub value: String,
    pub insertion_line: isize,
    pub selected: (usize, usize),
    pub marked: Option<(usize, usize)>,
}

/// Runs one hook text operation through the view's Objective-C selectors, as an input
/// method or VoiceOver would. Inputs: the view and the operation. Output: the report of an
/// accessibility read, none for the others.
pub fn hook_text(view: &TermView, input: &HookInput) -> Option<AccessReport> {
    assert!(input.text_op());
    let none = range_or_none(None);
    match input {
        HookInput::Marked { text, at } => {
            let string = NSString::from_str(text);
            let selected = NSRange::new(*at, 0);
            let _: () = unsafe {
                msg_send![
                    view,
                    setMarkedText: &*string,
                    selectedRange: selected,
                    replacementRange: none
                ]
            };
            None
        }
        HookInput::Insert { text } => {
            let string = NSString::from_str(text);
            let _: () = unsafe { msg_send![view, insertText: &*string, replacementRange: none] };
            None
        }
        HookInput::Unmark => {
            let _: () = unsafe { msg_send![view, unmarkText] };
            None
        }
        HookInput::Access => Some(access_report(view)),
        _ => unreachable!("only text operations reach hook_text"),
    }
}

/// Reads a view's accessibility and input method state through its selectors. Input: the
/// view. Output: the report.
fn access_report(view: &TermView) -> AccessReport {
    let value: Option<Retained<NSString>> = unsafe { msg_send![view, accessibilityValue] };
    let role: Option<Retained<NSString>> = unsafe { msg_send![view, accessibilityRole] };
    let line: NSInteger = unsafe { msg_send![view, accessibilityInsertionPointLineNumber] };
    let selected: NSRange = unsafe { msg_send![view, accessibilitySelectedTextRange] };
    let marked: NSRange = unsafe { msg_send![view, markedRange] };
    let shown = marked.location != NSNotFound as usize;
    assert!(shown || marked.length == 0);
    AccessReport {
        input_context: view.inputContext().is_some(),
        role: role.map(|s| s.to_string()).unwrap_or_default(),
        value: value.map(|s| s.to_string()).unwrap_or_default(),
        insertion_line: line,
        selected: (selected.location, selected.length),
        marked: shown.then_some((marked.location, marked.length)),
    }
}
