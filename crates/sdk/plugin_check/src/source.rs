use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use proc_macro2::{Delimiter, Span, TokenStream, TokenTree};

use kernel_plugin_host::manifest::{Bounded, read_bounded};

use crate::Violation;

/// The modules of `core` and `alloc` a plugin may name: data types, traits and formatting,
/// nothing that reaches files, network, processes, the environment or threads.
pub const STD_ALLOWED: [&str; 18] = [
    "borrow",
    "boxed",
    "cmp",
    "collections",
    "convert",
    "default",
    "fmt",
    "hash",
    "iter",
    "marker",
    "num",
    "ops",
    "option",
    "result",
    "slice",
    "str",
    "string",
    "vec",
];
/// The macros a plugin may call, by name; any other macro call is refused.
pub const MACROS_ALLOWED: [&str; 23] = [
    "assert",
    "assert_eq",
    "assert_ne",
    "debug_assert",
    "debug_assert_eq",
    "debug_assert_ne",
    "format",
    "write",
    "writeln",
    "vec",
    "matches",
    "panic",
    "unreachable",
    "todo",
    "unimplemented",
    "concat",
    "stringify",
    "cfg",
    "line",
    "column",
    "file",
    "module_path",
    "compile_error",
];
/// Names refused wherever they appear, so no alias import reaches them: the compile-time
/// file and environment readers, inline assembly and `macro_rules`.
const NAMES_NOT_ALLOWED: [&str; 9] = [
    "include",
    "include_str",
    "include_bytes",
    "env",
    "option_env",
    "asm",
    "global_asm",
    "naked_asm",
    "macro_rules",
];
/// The attributes a plugin may write, by name; any other attribute is refused.
pub const ATTRS_ALLOWED: [&str; 14] = [
    "doc",
    "derive",
    "cfg",
    "test",
    "inline",
    "must_use",
    "allow",
    "warn",
    "deny",
    "forbid",
    "expect",
    "non_exhaustive",
    "repr",
    "no_std",
];
/// The keywords that may stand before `!` and a group without being a macro call, as in
/// `if !(a)` or `return !(a)`.
const KEYWORDS: [&str; 8] = [
    "if", "while", "return", "match", "in", "break", "else", "yield",
];
/// The first segments a `use` may start with.
const USE_ROOTS: [&str; 6] = ["sdk", "core", "alloc", "crate", "self", "super"];
/// The crate roots a path may name, each only with a second segment from the allowlists.
const ROOTS: [&str; 2] = ["core", "alloc"];
/// The exact first lines of every plugin's `src/lib.rs`.
pub const HEADER: &str = "#![no_std]\n#![forbid(unsafe_code)]\n";
/// The most `.rs` files one plugin's `src` may hold.
pub(crate) const SOURCE_FILES_MAX: usize = 256;
/// The most bytes one plugin source file may hold.
pub(crate) const SOURCE_BYTES_MAX: usize = 1_048_576;
/// The deepest folder nesting below `src` the scan walks.
const DEPTH_MAX: usize = 16;
/// The deepest token group nesting the scan walks.
const GROUP_DEPTH_MAX: usize = 256;

/// Scans every `.rs` file below the crate's `src` with one token pass per file, and checks
/// that `src/lib.rs` starts with `HEADER`. Inputs: the crate folder and the violation list.
/// Output: the list grows.
pub(crate) fn sources(dir: &Path, out: &mut Vec<Violation>) {
    let files = rust_files(dir, out);
    let lib = PathBuf::from("src/lib.rs");
    if !files.contains(&lib) {
        out.push(at(&lib, 1, "plugin_header_missing"));
    }
    for file in files {
        let text = match read_bounded(&dir.join(&file), SOURCE_BYTES_MAX) {
            Bounded::Text(text) => text,
            Bounded::TooLarge => {
                out.push(at(&file, 1, "plugin_source_too_large"));
                continue;
            }
            Bounded::Unreadable => {
                out.push(at(&file, 1, "plugin_source_unparsed"));
                continue;
            }
        };
        if let Err(error) = syn::parse_file(&text) {
            let line = error.span().start().line;
            out.push(at(&file, line, "plugin_source_unparsed"));
            continue;
        }
        let Ok(stream) = TokenStream::from_str(&text) else {
            out.push(at(&file, 1, "plugin_source_unparsed"));
            continue;
        };
        if file == lib && !text.starts_with(HEADER) {
            out.push(at(&file, 1, "plugin_header_missing"));
        }
        let mut scan = Scan {
            file: &file,
            out: &mut *out,
        };
        scan.walk(&stream, Mode::Code, 0);
    }
}

/// Lists the `.rs` files below `src`, relative to the crate folder, sorted; a symlink, a
/// file not named exactly `*.rs` (a case-insensitive file system resolves `mod m;` to
/// `m.RS`), a folder past `DEPTH_MAX` or more than `SOURCE_FILES_MAX` files is a violation.
/// Inputs: the crate folder and the violation list. Output: the files.
fn rust_files(dir: &Path, out: &mut Vec<Violation>) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut folders = vec![(PathBuf::from("src"), 0usize)];
    while let Some((folder, depth)) = folders.pop() {
        let Ok(entries) = fs::read_dir(dir.join(&folder)) else {
            continue;
        };
        let mut names: Vec<PathBuf> = entries
            .flatten()
            .filter(|entry| entry.file_name() != crate::FINDER_FILE)
            .map(|entry| folder.join(entry.file_name()))
            .collect();
        names.sort();
        for name in names {
            let Ok(meta) = fs::symlink_metadata(dir.join(&name)) else {
                continue;
            };
            if meta.file_type().is_symlink() {
                out.push(at(&name, 1, "plugin_manifest_not_allowed"));
            } else if meta.is_dir() {
                if depth + 1 > DEPTH_MAX {
                    out.push(at(&name, 1, "plugin_manifest_not_allowed"));
                } else {
                    folders.push((name, depth + 1));
                }
            } else if is_rust_file(&name) {
                files.push(name);
            } else {
                out.push(at(&name, 1, "plugin_manifest_not_allowed"));
            }
        }
        if files.len() > SOURCE_FILES_MAX {
            out.push(at(&folder, 1, "plugin_source_too_large"));
            files.truncate(SOURCE_FILES_MAX);
            break;
        }
    }
    files.sort();
    assert!(files.len() <= SOURCE_FILES_MAX);
    files
}

/// Whether a file name ends in exactly `.rs` with a stem before it. Input: the path.
/// Output: true for a Rust source name.
fn is_rust_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.len() > 3 && name.ends_with(".rs"))
}

/// Builds a violation. Inputs: the file, line and code. Output: the violation.
fn at(file: &Path, line: usize, code: &'static str) -> Violation {
    Violation {
        file: file.to_path_buf(),
        line,
        code,
    }
}

/// Where a token stream sits: plain code, the arguments of a macro call, or the inside of
/// an attribute. Macro arguments and attribute insides are opaque to the parser, so items
/// and attributes there are refused outright.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Code,
    Opaque,
}

/// The token pass over one file, adding each refused token it meets.
struct Scan<'a> {
    file: &'a Path,
    out: &'a mut Vec<Violation>,
}

impl Scan<'_> {
    /// Adds one violation at a span. Inputs: the span and the code. Output: the list grows.
    fn push(&mut self, span: Span, code: &'static str) {
        self.out.push(at(self.file, span.start().line, code));
    }

    /// Walks one token stream and every group inside it. Inputs: the tokens, where they
    /// sit and the group depth. Output: violations added.
    fn walk(&mut self, stream: &TokenStream, mode: Mode, depth: usize) {
        let tokens: Vec<TokenTree> = stream.clone().into_iter().collect();
        if depth > GROUP_DEPTH_MAX {
            if let Some(first) = tokens.first() {
                self.push(first.span(), "plugin_source_too_large");
            }
            return;
        }
        let mut index = 0;
        while index < tokens.len() {
            index = self.token(&tokens, index, mode, depth);
        }
    }

    /// Checks the token at one index and walks what it opens. Inputs: the tokens, the
    /// index, the mode and the group depth. Output: the index of the next token to check.
    fn token(&mut self, tokens: &[TokenTree], index: usize, mode: Mode, depth: usize) -> usize {
        match &tokens[index] {
            TokenTree::Group(group) => {
                self.walk(&group.stream(), mode, depth + 1);
                index + 1
            }
            TokenTree::Punct(punct) if punct.as_char() == '#' => {
                self.attribute(tokens, index, mode, depth)
            }
            TokenTree::Punct(_) | TokenTree::Literal(_) => index + 1,
            TokenTree::Ident(ident) => {
                let name = ident.to_string();
                if name.starts_with("r#") {
                    self.push(ident.span(), "plugin_raw_ident");
                    return index + 1;
                }
                self.ident(tokens, index, &name, mode, depth)
            }
        }
    }

    /// Checks one plain identifier: `std`, `unsafe`, the refused names, a crate root and its
    /// second segment, `use` and `extern crate`, items inside macro arguments, and a macro
    /// call's name. Inputs: the tokens, the index, the name, the mode and the group depth.
    /// Output: the index of the next token to check.
    fn ident(
        &mut self,
        tokens: &[TokenTree],
        index: usize,
        name: &str,
        mode: Mode,
        depth: usize,
    ) -> usize {
        let span = tokens[index].span();
        match name {
            "std" => self.push(span, "plugin_std_not_allowed"),
            "unsafe" => self.push(span, "plugin_unsafe"),
            "use" | "extern" | "mod" if mode == Mode::Opaque => {
                self.push(span, "plugin_macro_item_not_allowed");
            }
            "use" => self.use_roots(tokens.get(index + 1..).unwrap_or_default()),
            "extern" if is_ident(tokens.get(index + 1), "crate") => {
                return self.extern_crate(tokens, index);
            }
            _ => {}
        }
        if NAMES_NOT_ALLOWED.contains(&name) {
            self.push(span, "plugin_macro_not_allowed");
        }
        if ROOTS.contains(&name) {
            self.root(tokens, index);
        }
        let called = !KEYWORDS.contains(&name)
            && is_punct(tokens.get(index + 1), '!')
            && matches!(tokens.get(index + 2), Some(TokenTree::Group(_)));
        if !called {
            return index + 1;
        }
        if !MACROS_ALLOWED.contains(&name) && !NAMES_NOT_ALLOWED.contains(&name) {
            self.push(span, "plugin_macro_not_allowed");
        }
        if let Some(TokenTree::Group(group)) = tokens.get(index + 2) {
            self.walk(&group.stream(), Mode::Opaque, depth + 1);
        }
        index + 3
    }

    /// Checks a crate root's second segment: `core` and `alloc` are named only as
    /// `root::name` with `name` an allowed module or macro, wherever the root sits, so a
    /// rename through `crate::` or `self::` is refused too. Inputs: the tokens and the root's
    /// index. Output: a violation when the form is any other.
    fn root(&mut self, tokens: &[TokenTree], index: usize) {
        let path = is_punct(tokens.get(index + 1), ':') && is_punct(tokens.get(index + 2), ':');
        match tokens.get(index + 3) {
            Some(TokenTree::Ident(next)) if path => {
                let next = next.to_string();
                let allowed = STD_ALLOWED.contains(&next.as_str())
                    || MACROS_ALLOWED.contains(&next.as_str())
                    || NAMES_NOT_ALLOWED.contains(&next.as_str())
                    || next.starts_with("r#");
                if !allowed {
                    self.push(tokens[index + 3].span(), "plugin_std_not_allowed");
                }
            }
            _ => self.push(tokens[index].span(), "plugin_std_not_allowed"),
        }
    }

    /// Checks the first segment of each tree of a `use`: one of `USE_ROOTS`, never a leading
    /// `::`; a `std` start is left to the `std` rule. Input: the tokens after `use`.
    /// Output: violations added.
    fn use_roots(&mut self, rest: &[TokenTree]) {
        match rest.first() {
            Some(TokenTree::Group(group)) if group.delimiter() == Delimiter::Brace => {
                let inner: Vec<TokenTree> = group.stream().into_iter().collect();
                for part in inner.split(|token| is_punct(Some(token), ',')) {
                    self.use_root(part);
                }
            }
            _ => {
                let end = rest
                    .iter()
                    .position(|token| is_punct(Some(token), ';'))
                    .unwrap_or(rest.len());
                self.use_root(&rest[..end]);
            }
        }
    }

    /// Checks one `use` tree's first segment. Input: the tree's tokens. Output: a violation
    /// when it starts with `::` or a name outside `USE_ROOTS`.
    fn use_root(&mut self, tree: &[TokenTree]) {
        let leading = is_punct(tree.first(), ':');
        if leading && let Some(first) = tree.first() {
            let name = tree.iter().find_map(|token| match token {
                TokenTree::Ident(ident) => Some(ident.to_string()),
                _ => None,
            });
            if name.as_deref() != Some("std") {
                self.push(first.span(), "plugin_use_not_allowed");
            }
            return;
        }
        let first = tree.iter().find(|token| !is_punct(Some(token), ':'));
        let Some(TokenTree::Ident(ident)) = first else {
            return;
        };
        let name = ident.to_string();
        if name == "std" || name.starts_with("r#") {
            return;
        }
        if leading || !USE_ROOTS.contains(&name.as_str()) {
            self.push(ident.span(), "plugin_use_not_allowed");
        }
    }

    /// Checks an `extern crate` item: only the exact `extern crate alloc;` passes.
    /// Inputs: the tokens and the index of `extern`. Output: the index after the item.
    fn extern_crate(&mut self, tokens: &[TokenTree], index: usize) -> usize {
        let exact =
            is_ident(tokens.get(index + 2), "alloc") && is_punct(tokens.get(index + 3), ';');
        if exact {
            return index + 4;
        }
        self.push(tokens[index].span(), "plugin_extern_crate");
        tokens[index..]
            .iter()
            .position(|token| is_punct(Some(token), ';'))
            .map_or(tokens.len(), |end| index + end + 1)
    }

    /// Checks an attribute `#[..]` or `#![..]`: refused inside macro arguments, else its name
    /// must be in `ATTRS_ALLOWED` (`path` has its own code); its inside is walked as opaque.
    /// Inputs: the tokens, the index of `#`, the mode and the group depth. Output: the index
    /// after the attribute.
    fn attribute(&mut self, tokens: &[TokenTree], index: usize, mode: Mode, depth: usize) -> usize {
        let inner = usize::from(is_punct(tokens.get(index + 1), '!'));
        let Some(TokenTree::Group(group)) = tokens.get(index + 1 + inner) else {
            return index + 1;
        };
        if group.delimiter() != Delimiter::Bracket {
            return index + 1;
        }
        if mode == Mode::Opaque {
            self.push(tokens[index].span(), "plugin_macro_item_not_allowed");
            return index + 2 + inner;
        }
        match group.stream().into_iter().next() {
            Some(TokenTree::Ident(head)) => {
                let name = head.to_string();
                if name == "path" {
                    self.push(head.span(), "plugin_path_attr");
                } else if !name.starts_with("r#") && !ATTRS_ALLOWED.contains(&name.as_str()) {
                    self.push(head.span(), "plugin_attr_not_allowed");
                }
            }
            Some(head) => self.push(head.span(), "plugin_attr_not_allowed"),
            None => self.push(tokens[index].span(), "plugin_attr_not_allowed"),
        }
        self.walk(&group.stream(), Mode::Opaque, depth + 1);
        index + 2 + inner
    }
}

/// Whether a token is a given punctuation mark. Inputs: the token, if any, and the mark.
/// Output: true when it is that mark.
fn is_punct(token: Option<&TokenTree>, mark: char) -> bool {
    matches!(token, Some(TokenTree::Punct(punct)) if punct.as_char() == mark)
}

/// Whether a token is a given plain identifier. Inputs: the token, if any, and the name.
/// Output: true when it is that identifier.
fn is_ident(token: Option<&TokenTree>, name: &str) -> bool {
    matches!(token, Some(TokenTree::Ident(ident)) if ident == name)
}
