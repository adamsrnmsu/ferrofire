//! expr::compile: any text parses or is a ParseError; a parsed tree evaluates without panicking.
#![no_main]
use ferrofire::expr::compile;
use libfuzzer_sys::fuzz_target;
use std::collections::HashMap;

fuzz_target!(|src: &str| {
    let syms: HashMap<String, usize> = [("a", 0), ("b", 1), ("level", 2)]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    if let Ok(n) = compile(src, &syms) {
        let _ = n.eval(&[1.5, -2.0, 0.0]);
        let mut vars = vec![];
        n.vars(&mut vars);
    }
});
