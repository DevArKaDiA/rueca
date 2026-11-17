//! Names that are always in scope in Rust (the prelude), the standard
//! library names code commonly uses, primitive types, and the standard
//! macros. The resolver never binds them to a project symbol by unique name
//! (`rueca_extract::LanguagePack::is_builtin`).

/// Primitive types (never references).
const PRIMITIVES: &[&str] = &[
    "bool", "char", "str", "u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64", "i128", "isize",
    "f32", "f64",
];

/// The Rust 2021 / 2024 prelude, crate roots of the standard library and the
/// standard types most code names. Sorted (binary search).
const PRELUDE: &[&str] = &[
    "Arc",
    "AsMut",
    "AsRef",
    "BTreeMap",
    "BTreeSet",
    "BinaryHeap",
    "Box",
    "Cell",
    "Clone",
    "Copy",
    "Cow",
    "Debug",
    "Default",
    "Display",
    "DoubleEndedIterator",
    "Drop",
    "Duration",
    "Eq",
    "Err",
    "Error",
    "ExactSizeIterator",
    "Extend",
    "Fn",
    "FnMut",
    "FnOnce",
    "From",
    "FromIterator",
    "FromStr",
    "Future",
    "Hash",
    "HashMap",
    "HashSet",
    "Instant",
    "Into",
    "IntoFuture",
    "IntoIterator",
    "Iterator",
    "LinkedList",
    "Mutex",
    "NonNull",
    "None",
    "Ok",
    "Option",
    "Ord",
    "PartialEq",
    "PartialOrd",
    "Path",
    "PathBuf",
    "PhantomData",
    "Pin",
    "Rc",
    "RefCell",
    "Result",
    "RwLock",
    "Send",
    "Sized",
    "Some",
    "String",
    "Sync",
    "ToOwned",
    "ToString",
    "TryFrom",
    "TryInto",
    "Unpin",
    "Vec",
    "VecDeque",
    "Weak",
    "alloc",
    "core",
    "drop",
    "std",
];

/// Standard-library macros (and the attribute-like `cfg`). Invocations of
/// these are not references unless the file defines a `macro_rules!` with
/// the same name. Sorted.
const STD_MACROS: &[&str] = &[
    "addr_of",
    "addr_of_mut",
    "assert",
    "assert_eq",
    "assert_matches",
    "assert_ne",
    "cfg",
    "column",
    "compile_error",
    "concat",
    "concat_bytes",
    "const_format_args",
    "dbg",
    "debug_assert",
    "debug_assert_eq",
    "debug_assert_matches",
    "debug_assert_ne",
    "env",
    "eprint",
    "eprintln",
    "file",
    "format",
    "format_args",
    "include",
    "include_bytes",
    "include_str",
    "is_aarch64_feature_detected",
    "is_x86_feature_detected",
    "line",
    "matches",
    "module_path",
    "option_env",
    "panic",
    "pin",
    "print",
    "println",
    "ready",
    "stringify",
    "thread_local",
    "todo",
    "try",
    "unimplemented",
    "unreachable",
    "vec",
    "write",
    "writeln",
];

/// Macros whose arguments are not Rust code that rust-analyzer resolves:
/// strings and tokens turned into data (`stringify!`, `concat!`, `include*`)
/// or token streams built for a procedural macro (`quote!`). Their
/// arguments are not re-parsed.
const OPAQUE_MACROS: &[&str] = &[
    "compile_error",
    "concat",
    "concat_idents",
    "include",
    "include_bytes",
    "include_str",
    "parse_quote",
    "parse_quote_spanned",
    "quote",
    "quote_spanned",
    "stringify",
];

/// Whether `name` is a primitive type.
pub(crate) fn is_primitive(name: &str) -> bool {
    PRIMITIVES.contains(&name)
}

/// Whether `name` is always in scope (prelude, primitive, standard crate root).
pub(crate) fn is_builtin(name: &str) -> bool {
    is_primitive(name) || PRELUDE.binary_search(&name).is_ok()
}

/// Whether `name` is a standard-library macro.
pub(crate) fn is_std_macro(name: &str) -> bool {
    STD_MACROS.binary_search(&name).is_ok()
}

/// Whether the arguments of macro `name` are not code (see [`OPAQUE_MACROS`]).
pub(crate) fn is_opaque_macro(name: &str) -> bool {
    OPAQUE_MACROS.contains(&name)
}

/// Smart pointers and references that dereference to their first type
/// argument: a method called on them is a method of the pointee.
pub(crate) fn is_deref_wrapper(name: &str) -> bool {
    matches!(name, "Box" | "Rc" | "Arc" | "Cow" | "Pin" | "ManuallyDrop" | "Ref" | "RefMut" | "MutexGuard")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_are_sorted_and_answer() {
        assert!(PRELUDE.windows(2).all(|w| w[0] < w[1]), "PRELUDE must be sorted");
        assert!(STD_MACROS.windows(2).all(|w| w[0] < w[1]), "STD_MACROS must be sorted");
        assert!(is_builtin("Vec") && is_builtin("u8") && is_builtin("Some") && is_builtin("std"));
        assert!(!is_builtin("Linker") && !is_builtin("helper"));
        assert!(is_std_macro("println") && is_std_macro("vec") && !is_std_macro("my_macro"));
        assert!(is_opaque_macro("quote") && !is_opaque_macro("format"));
        assert!(is_deref_wrapper("Arc") && !is_deref_wrapper("Option"));
    }
}
