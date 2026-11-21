use std::fs;
use std::path::{Path, PathBuf};

use proc_macro2::{Span, TokenStream, TokenTree};
use syn::visit::{self, Visit};

use crate::Violation;

/// The modules of `std`, `core` and `alloc` a plugin may name: data types, traits and
/// formatting, nothing that reaches files, network, processes, the environment or threads.
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
/// The crate roots whose paths the allowlist governs.
const ROOTS: [&str; 3] = ["std", "core", "alloc"];
/// Macros a plugin may not call: file and environment readers, inline assembly, the
/// prelude's output macros, and `macro_rules` (its bodies are not scanned as code).
const MACROS_NOT_ALLOWED: [&str; 13] = [
    "include",
    "include_str",
    "include_bytes",
    "env",
    "option_env",
    "asm",
    "global_asm",
    "println",
    "print",
    "eprintln",
    "eprint",
    "dbg",
    "macro_rules",
];
/// Attributes that export or place a symbol, unsafe in effect.
const ATTRS_UNSAFE: [&str; 3] = ["no_mangle", "export_name", "link_section"];
/// The most `.rs` files one plugin's `src` may hold.
pub(crate) const SOURCE_FILES_MAX: usize = 256;
/// The most bytes one plugin source file may hold.
pub(crate) const SOURCE_BYTES_MAX: usize = 1_048_576;
/// The deepest folder nesting below `src` the scan walks.
const DEPTH_MAX: usize = 16;

/// Scans every `.rs` file below the crate's `src`, and checks that `src/lib.rs` forbids
/// unsafe code. Inputs: the crate folder and the violation list. Output: the list grows.
pub(crate) fn sources(dir: &Path, out: &mut Vec<Violation>) {
    let files = rust_files(dir, out);
    let lib = PathBuf::from("src/lib.rs");
    if !files.contains(&lib) {
        out.push(at(&lib, 1, "plugin_forbid_missing"));
    }
    for file in files {
        let Some(text) = fs::read_to_string(dir.join(&file))
            .ok()
            .filter(|text| text.len() <= SOURCE_BYTES_MAX)
        else {
            out.push(at(&file, 1, "plugin_source_too_large"));
            continue;
        };
        let parsed = match syn::parse_file(&text) {
            Ok(parsed) => parsed,
            Err(error) => {
                out.push(at(
                    &file,
                    error.span().start().line,
                    "plugin_source_unparsed",
                ));
                continue;
            }
        };
        if file == lib && !forbids_unsafe(&parsed) {
            out.push(at(&file, 1, "plugin_forbid_missing"));
        }
        let mut scan = Scan {
            file: &file,
            out: &mut *out,
        };
        scan.visit_file(&parsed);
    }
}

/// Lists the `.rs` files below `src`, relative to the crate folder, sorted; a symlink, a
/// folder past `DEPTH_MAX` or more than `SOURCE_FILES_MAX` files is a violation.
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
            } else if name.extension().is_some_and(|ext| ext == "rs") {
                files.push(name);
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

/// Whether a file's inner attributes hold `forbid(unsafe_code)`. Input: the parsed file.
/// Output: true when they do.
fn forbids_unsafe(file: &syn::File) -> bool {
    file.attrs.iter().any(|attr| {
        matches!(attr.style, syn::AttrStyle::Inner(_))
            && attr.path().is_ident("forbid")
            && matches!(&attr.meta, syn::Meta::List(list)
                if list.tokens.clone().into_iter().any(|token| matches!(token, TokenTree::Ident(ident) if ident == "unsafe_code")))
    })
}

/// Builds a violation. Inputs: the file, line and code. Output: the violation.
fn at(file: &Path, line: usize, code: &'static str) -> Violation {
    Violation {
        file: file.to_path_buf(),
        line,
        code,
    }
}

/// The visitor over one parsed file, adding each forbidden item it meets.
struct Scan<'a> {
    file: &'a Path,
    out: &'a mut Vec<Violation>,
}

impl Scan<'_> {
    /// Adds one violation at a span. Inputs: the span and the code. Output: the list grows.
    fn push(&mut self, span: Span, code: &'static str) {
        self.out.push(at(self.file, span.start().line, code));
    }

    /// Checks the module named right after a crate root. Inputs: the name and its span.
    /// Output: a violation when the module is outside `STD_ALLOWED`.
    fn second(&mut self, name: &str, span: Span) {
        if !STD_ALLOWED.contains(&name) {
            self.push(span, "plugin_std_not_allowed");
        }
    }

    /// Checks a `use` tree at the top level of a `use` item: a crate root may only be
    /// followed by an allowed module, and is never renamed, globbed or used whole.
    /// Input: the tree. Output: violations added.
    fn use_top(&mut self, tree: &syn::UseTree) {
        match tree {
            syn::UseTree::Path(path) if is_root(&path.ident) => self.use_second(&path.tree),
            syn::UseTree::Name(name) if is_root(&name.ident) => {
                self.push(name.ident.span(), "plugin_std_not_allowed");
            }
            syn::UseTree::Rename(rename) if is_root(&rename.ident) => {
                self.push(rename.ident.span(), "plugin_std_not_allowed");
            }
            syn::UseTree::Group(group) => {
                for item in &group.items {
                    self.use_top(item);
                }
            }
            _ => {}
        }
    }

    /// Checks the tree right after a crate root in a `use`. Input: the tree.
    /// Output: violations added.
    fn use_second(&mut self, tree: &syn::UseTree) {
        match tree {
            syn::UseTree::Path(path) => self.second(&path.ident.to_string(), path.ident.span()),
            syn::UseTree::Name(name) => self.second(&name.ident.to_string(), name.ident.span()),
            syn::UseTree::Rename(rename) => {
                self.second(&rename.ident.to_string(), rename.ident.span());
            }
            syn::UseTree::Glob(glob) => self.push(glob.star_token.span, "plugin_std_not_allowed"),
            syn::UseTree::Group(group) => {
                for item in &group.items {
                    self.use_second(item);
                }
            }
        }
    }

    /// Scans a token stream the parser does not read as code (macro input, attribute
    /// arguments): a crate root followed by `::` and a module, a forbidden macro name
    /// followed by `!`, and the word `unsafe`. Input: the tokens. Output: violations added.
    fn tokens(&mut self, stream: &TokenStream) {
        let tokens: Vec<TokenTree> = stream.clone().into_iter().collect();
        for (index, token) in tokens.iter().enumerate() {
            match token {
                TokenTree::Group(group) => self.tokens(&group.stream()),
                TokenTree::Ident(ident) => {
                    let name = ident.to_string();
                    if name == "unsafe" {
                        self.push(ident.span(), "plugin_unsafe");
                    }
                    if is_punct(tokens.get(index + 1), '!')
                        && MACROS_NOT_ALLOWED.contains(&name.as_str())
                    {
                        self.push(ident.span(), "plugin_macro_not_allowed");
                    }
                    if ROOTS.contains(&name.as_str())
                        && is_punct(tokens.get(index + 1), ':')
                        && is_punct(tokens.get(index + 2), ':')
                    {
                        match tokens.get(index + 3) {
                            Some(TokenTree::Ident(next)) => {
                                self.second(&next.to_string(), next.span())
                            }
                            _ => self.push(ident.span(), "plugin_std_not_allowed"),
                        }
                    }
                }
                TokenTree::Punct(_) | TokenTree::Literal(_) => {}
            }
        }
    }
}

impl<'ast> Visit<'ast> for Scan<'_> {
    fn visit_signature(&mut self, sig: &'ast syn::Signature) {
        if let Some(token) = &sig.unsafety {
            self.push(token.span, "plugin_unsafe");
        }
        visit::visit_signature(self, sig);
    }

    fn visit_item_impl(&mut self, item: &'ast syn::ItemImpl) {
        if let Some(token) = &item.unsafety {
            self.push(token.span, "plugin_unsafe");
        }
        visit::visit_item_impl(self, item);
    }

    fn visit_item_trait(&mut self, item: &'ast syn::ItemTrait) {
        if let Some(token) = &item.unsafety {
            self.push(token.span, "plugin_unsafe");
        }
        visit::visit_item_trait(self, item);
    }

    fn visit_expr_unsafe(&mut self, expr: &'ast syn::ExprUnsafe) {
        self.push(expr.unsafe_token.span, "plugin_unsafe");
        visit::visit_expr_unsafe(self, expr);
    }

    fn visit_item_foreign_mod(&mut self, item: &'ast syn::ItemForeignMod) {
        self.push(item.abi.extern_token.span, "plugin_unsafe");
        visit::visit_item_foreign_mod(self, item);
    }

    fn visit_item_extern_crate(&mut self, item: &'ast syn::ItemExternCrate) {
        self.push(item.extern_token.span, "plugin_extern_crate");
        visit::visit_item_extern_crate(self, item);
    }

    fn visit_attribute(&mut self, attr: &'ast syn::Attribute) {
        let path = attr.path();
        if path.is_ident("path") {
            self.push(attr.pound_token.span, "plugin_path_attr");
        }
        if ATTRS_UNSAFE.iter().any(|name| path.is_ident(name)) {
            self.push(attr.pound_token.span, "plugin_unsafe");
        }
        if let syn::Meta::List(list) = &attr.meta {
            self.tokens(&list.tokens);
        }
        visit::visit_attribute(self, attr);
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        if let Some(last) = mac.path.segments.last()
            && MACROS_NOT_ALLOWED.contains(&last.ident.to_string().as_str())
        {
            self.push(last.ident.span(), "plugin_macro_not_allowed");
        }
        self.tokens(&mac.tokens);
        visit::visit_macro(self, mac);
    }

    fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
        self.use_top(&item.tree);
    }

    fn visit_path(&mut self, path: &'ast syn::Path) {
        let mut segments = path.segments.iter();
        if let (Some(first), Some(second)) = (segments.next(), segments.next())
            && is_root(&first.ident)
        {
            self.second(&second.ident.to_string(), second.ident.span());
        }
        visit::visit_path(self, path);
    }
}

/// Whether an identifier is one of the crate roots `std`, `core` or `alloc`.
/// Input: the identifier. Output: true for a root.
fn is_root(ident: &proc_macro2::Ident) -> bool {
    ROOTS.iter().any(|root| ident == root)
}

/// Whether a token is a given punctuation mark. Inputs: the token, if any, and the mark.
/// Output: true when it is that mark.
fn is_punct(token: Option<&TokenTree>, mark: char) -> bool {
    matches!(token, Some(TokenTree::Punct(punct)) if punct.as_char() == mark)
}
