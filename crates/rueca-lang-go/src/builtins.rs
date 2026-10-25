//! Go predeclared identifiers and keywords (The Go Programming Language
//! Specification, "Predeclared identifiers" and "Keywords").

/// Predeclared types, constants, the zero value and functions, sorted.
const PREDECLARED: &[&str] = &[
    "any",
    "append",
    "bool",
    "byte",
    "cap",
    "clear",
    "close",
    "comparable",
    "complex",
    "complex128",
    "complex64",
    "copy",
    "delete",
    "error",
    "false",
    "float32",
    "float64",
    "imag",
    "int",
    "int16",
    "int32",
    "int64",
    "int8",
    "iota",
    "len",
    "make",
    "max",
    "min",
    "new",
    "nil",
    "panic",
    "print",
    "println",
    "real",
    "recover",
    "rune",
    "string",
    "true",
    "uint",
    "uint16",
    "uint32",
    "uint64",
    "uint8",
    "uintptr",
];

/// Predeclared types (a subset of [`PREDECLARED`]).
const TYPES: &[&str] = &[
    "any",
    "bool",
    "byte",
    "comparable",
    "complex128",
    "complex64",
    "error",
    "float32",
    "float64",
    "int",
    "int16",
    "int32",
    "int64",
    "int8",
    "rune",
    "string",
    "uint",
    "uint16",
    "uint32",
    "uint64",
    "uint8",
    "uintptr",
];

const KEYWORDS: &[&str] = &[
    "break",
    "case",
    "chan",
    "const",
    "continue",
    "default",
    "defer",
    "else",
    "fallthrough",
    "for",
    "func",
    "go",
    "goto",
    "if",
    "import",
    "interface",
    "map",
    "package",
    "range",
    "return",
    "select",
    "struct",
    "switch",
    "type",
    "var",
];

/// Whether `name` is a predeclared identifier (`len`, `string`, `nil`, …).
pub(crate) fn is_predeclared(name: &str) -> bool {
    PREDECLARED.binary_search(&name).is_ok()
}

/// Whether `name` is a predeclared type (`int`, `error`, `any`, …).
pub(crate) fn is_predeclared_type(name: &str) -> bool {
    TYPES.binary_search(&name).is_ok()
}

pub(crate) fn is_keyword(name: &str) -> bool {
    KEYWORDS.binary_search(&name).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_are_sorted_and_consistent() {
        for t in [PREDECLARED, TYPES, KEYWORDS] {
            assert!(t.windows(2).all(|w| w[0] < w[1]), "{t:?}");
        }
        assert!(TYPES.iter().all(|t| is_predeclared(t)));
        assert!(is_predeclared("len") && is_predeclared("error") && !is_predeclared("Len"));
        assert!(is_keyword("go") && !is_keyword("gopher"));
    }
}
