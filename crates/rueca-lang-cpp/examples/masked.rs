//! Prints the ranges preprocessing rewrites (masked macros, dropped `#if`
//! branches, repairs): `cargo run -p rueca-lang-cpp --example masked -- <paths…>`
//! (`.c` files as C, everything else as C++).
fn main() {
    for path in std::env::args().skip(1) {
        let src = std::fs::read(&path).expect("read");
        let language = if path.ends_with(".c") { rueca_model::Language::C } else { rueca_model::Language::Cpp };
        let masked = rueca_lang_cpp::preprocessed(&src, language).expect("preprocess");
        let mut i = 0;
        while i < src.len() {
            if src[i] != masked[i] {
                let start = i;
                while i < src.len()
                    && (src[i] != masked[i] || (masked[i] == b' ' && i + 1 < src.len() && src[i + 1] != masked[i + 1]))
                {
                    i += 1;
                }
                let line = src[..start].iter().filter(|b| **b == b'\n').count() + 1;
                println!("{path}:{line}: {}", String::from_utf8_lossy(&src[start..i]).replace('\n', "\\n"));
            }
            i += 1;
        }
    }
}
