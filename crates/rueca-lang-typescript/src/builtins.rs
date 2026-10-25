//! Names that are always in scope in JavaScript / TypeScript code.
//!
//! [`BUILTINS`] is the union of ECMAScript standard globals, TypeScript
//! builtin / utility types, the ubiquitous Node.js and browser globals that are
//! rarely user-defined (`console`, `process`, `window`, `document`, ...), and
//! the Jest/Mocha test globals (`describe`, `it`, `expect`, ...); references
//! to them are not emitted at all.
//!
//! [`RUNTIME_GLOBALS`] are web-platform and runtime globals that projects
//! *may* also define (`Request`, `Response`, `Headers`, `Event`, `crypto`,
//! `Element`, …). The extractor still emits references to them (a project's
//! own `Request` must stay resolvable through imports and same-module
//! lookup), but the pack reports them as builtins to the resolver
//! ([`is_global`]), so an unbound `Request` is never matched to a project
//! symbol by unique name.
//!
//! Both slices are sorted (byte order) so lookups are a binary search; a unit
//! test enforces it.

pub(crate) static BUILTINS: &[&str] = &[
    "AbortController",
    "AbortSignal",
    "AggregateError",
    "Array",
    "ArrayBuffer",
    "ArrayBufferLike",
    "ArrayBufferView",
    "ArrayLike",
    "AsyncGenerator",
    "AsyncGeneratorFunction",
    "AsyncIterable",
    "AsyncIterableIterator",
    "AsyncIterator",
    "Atomics",
    "Awaited",
    "BigInt",
    "BigInt64Array",
    "BigUint64Array",
    "Boolean",
    "Buffer",
    "Capitalize",
    "ConstructorParameters",
    "DataView",
    "Date",
    "Error",
    "ErrorConstructor",
    "EvalError",
    "Exclude",
    "Extract",
    "FinalizationRegistry",
    "Float32Array",
    "Float64Array",
    "Function",
    "Generator",
    "GeneratorFunction",
    "Infinity",
    "InstanceType",
    "Int16Array",
    "Int32Array",
    "Int8Array",
    "Intl",
    "Iterable",
    "IterableIterator",
    "Iterator",
    "IteratorResult",
    "JSON",
    "Lowercase",
    "Map",
    "Math",
    "NaN",
    "NoInfer",
    "NodeJS",
    "NonNullable",
    "Number",
    "Object",
    "Omit",
    "OmitThisParameter",
    "Parameters",
    "Partial",
    "Pick",
    "Promise",
    "PromiseConstructor",
    "PromiseLike",
    "PropertyDescriptor",
    "PropertyKey",
    "Proxy",
    "RangeError",
    "Readonly",
    "ReadonlyArray",
    "ReadonlyMap",
    "ReadonlySet",
    "Record",
    "ReferenceError",
    "Reflect",
    "RegExp",
    "Required",
    "ReturnType",
    "Set",
    "SharedArrayBuffer",
    "String",
    "Symbol",
    "SyntaxError",
    "TemplateStringsArray",
    "TextDecoder",
    "TextEncoder",
    "ThisParameterType",
    "ThisType",
    "TypeError",
    "URIError",
    "URL",
    "URLSearchParams",
    "Uint16Array",
    "Uint32Array",
    "Uint8Array",
    "Uint8ClampedArray",
    "Uncapitalize",
    "Uppercase",
    "WeakMap",
    "WeakRef",
    "WeakSet",
    "__dirname",
    "__filename",
    "afterAll",
    "afterEach",
    "any",
    "arguments",
    "atob",
    "beforeAll",
    "beforeEach",
    "bigint",
    "boolean",
    "btoa",
    "clearImmediate",
    "clearInterval",
    "clearTimeout",
    "console",
    "decodeURI",
    "decodeURIComponent",
    "describe",
    "document",
    "encodeURI",
    "encodeURIComponent",
    "eval",
    "expect",
    "exports",
    "false",
    "fetch",
    "globalThis",
    "isFinite",
    "isNaN",
    "it",
    "jest",
    "localStorage",
    "module",
    "navigator",
    "never",
    "null",
    "number",
    "object",
    "parseFloat",
    "parseInt",
    "performance",
    "process",
    "queueMicrotask",
    "requestAnimationFrame",
    "require",
    "self",
    "sessionStorage",
    "setImmediate",
    "setInterval",
    "setTimeout",
    "string",
    "structuredClone",
    "super",
    "symbol",
    "test",
    "this",
    "true",
    "undefined",
    "unknown",
    "void",
    "window",
];

pub(crate) static RUNTIME_GLOBALS: &[&str] = &[
    "Blob",
    "BroadcastChannel",
    "Bun",
    "Cache",
    "CacheStorage",
    "CloseEvent",
    "CompressionStream",
    "Crypto",
    "CryptoKey",
    "CustomEvent",
    "DOMException",
    "DecompressionStream",
    "Deno",
    "Document",
    "DocumentFragment",
    "Element",
    "ErrorEvent",
    "Event",
    "EventSource",
    "EventTarget",
    "File",
    "FileList",
    "FileReader",
    "FormData",
    "HTMLElement",
    "Headers",
    "History",
    "Image",
    "IntersectionObserver",
    "KeyboardEvent",
    "Location",
    "MessageChannel",
    "MessageEvent",
    "MessagePort",
    "MouseEvent",
    "MutationObserver",
    "Navigator",
    "Node",
    "Notification",
    "PerformanceObserver",
    "ProgressEvent",
    "ReadableStream",
    "ReadableStreamDefaultReader",
    "Request",
    "RequestInfo",
    "RequestInit",
    "ResizeObserver",
    "Response",
    "ResponseInit",
    "Storage",
    "SubtleCrypto",
    "Text",
    "TextDecoderStream",
    "TextEncoderStream",
    "TransformStream",
    "WebSocket",
    "Window",
    "Worker",
    "WritableStream",
    "XMLHttpRequest",
    "caches",
    "crypto",
    "global",
    "history",
    "location",
];

/// Always-global names: references to them are not even emitted.
pub(crate) fn is_builtin(name: &str) -> bool {
    BUILTINS.binary_search(&name).is_ok()
}

/// Builtins plus [`RUNTIME_GLOBALS`]: what the resolver treats as builtin.
pub(crate) fn is_global(name: &str) -> bool {
    is_builtin(name) || RUNTIME_GLOBALS.binary_search(&name).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_sorted_and_unique() {
        for list in [BUILTINS, RUNTIME_GLOBALS] {
            for w in list.windows(2) {
                assert!(w[0] < w[1], "not sorted/unique at {:?} / {:?}", w[0], w[1]);
            }
        }
        for g in RUNTIME_GLOBALS {
            assert!(!is_builtin(g), "{g} is in both lists");
        }
        assert!(is_global("Request") && is_global("crypto") && is_global("console"));
        assert!(is_builtin("console"));
        assert!(is_builtin("Promise"));
        assert!(is_builtin("Record"));
        assert!(!is_builtin("UserService"));
        assert!(!is_builtin("Request"));
    }
}
