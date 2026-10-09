//! Property tests for the invariants of `dev/DIRECTIVES.md` §4 that need no
//! interpreter: builder output validates, printing and parsing round-trip
//! exactly, compaction preserves the text up to renumbering, and the validator
//! and parser never panic on mutated input.

mod common;

use common::{Rng, generate, lower};
use ir_lang::{Module, parse};
use proptest::prelude::*;

/// Applies one random token-level mutation to IR text.
fn mutate(text: &str, rng: &mut Rng) -> String {
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    if lines.len() < 2 {
        return text.to_string();
    }
    let i = 1 + rng.below(lines.len() as u64 - 1) as usize;
    match rng.below(9) {
        0 => {
            let _ = lines.remove(i);
        }
        1 => {
            let j = 1 + rng.below(lines.len() as u64 - 1) as usize;
            lines.swap(i, j);
        }
        2 => {
            let dup = lines[i].clone();
            lines.insert(i, dup);
        }
        _ => {
            // Replace one word with another word from the text.
            let words: Vec<String> = text
                .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .filter(|w| !w.is_empty())
                .map(str::to_string)
                .collect();
            let line_words: Vec<&str> = lines[i]
                .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .filter(|w| !w.is_empty())
                .collect();
            if !line_words.is_empty() && !words.is_empty() {
                let from = line_words[rng.below(line_words.len() as u64) as usize].to_string();
                let to = &words[rng.below(words.len() as u64) as usize];
                lines[i] = lines[i].replacen(&from, to, 1);
            }
        }
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// Invariant: a well-typed program lowered through the builder validates.
    #[test]
    fn prop_builder_output_validates(seed in any::<u64>()) {
        let low = lower(&generate(seed));
        prop_assert!(low.module.validate().is_ok(), "{:?}", low.module.validate());
    }

    /// Invariant: printing then parsing reproduces the module exactly (same text,
    /// so the same value, block, slot, and symbol handles), and the parse
    /// validates.
    #[test]
    fn prop_print_parse_round_trip(seed in any::<u64>()) {
        let low = lower(&generate(seed));
        let text = low.module.to_string();
        let again = parse(&text).unwrap();
        prop_assert_eq!(again.to_string(), text);
        prop_assert!(again.validate().is_ok());
    }

    /// Invariant: lowering is deterministic.
    #[test]
    fn prop_lowering_is_deterministic(seed in any::<u64>()) {
        let prog = generate(seed);
        prop_assert_eq!(lower(&prog).module.to_string(), lower(&prog).module.to_string());
    }

    /// Invariant: validation is total. Mutated text either fails to parse or
    /// parses to a module the validator accepts or rejects, never panicking, and
    /// a parsed module prints back to text that parses to the same module.
    #[test]
    fn prop_validator_never_panics_on_mutated_text(seed in any::<u64>(), mseed in any::<u64>(), rounds in 1usize..4) {
        let text = lower(&generate(seed)).module.to_string();
        let mut rng = Rng::new(mseed);
        let mut mutated = text;
        for _ in 0..rounds {
            mutated = mutate(&mutated, &mut rng);
        }
        if let Ok(m) = parse(&mutated) {
            let _ = m.validate();
            let printed = m.to_string();
            let again = parse(&printed).unwrap();
            prop_assert_eq!(again.to_string(), printed);
        }
    }

    /// The parser never panics on arbitrary bytes.
    #[test]
    fn prop_parser_never_panics_on_noise(text in r"\PC{0,200}") {
        let _ = parse(&text);
        let with_header = format!("module \"m\" ptr64\n{text}");
        let _ = parse(&with_header);
    }

    /// Invariant: compaction keeps the program and renumbers densely; a compacted
    /// module round-trips through text.
    #[test]
    fn prop_compact_round_trips(seed in any::<u64>()) {
        let mut low = lower(&generate(seed));
        let funcs: Vec<_> = low.module.functions().collect();
        for f in funcs {
            let mut b = low.module.edit(f).unwrap();
            let before_values = b.func().value_count();
            let remap = b.compact();
            prop_assert_eq!(b.func().value_count(), before_values);
            for v in 0..before_values {
                prop_assert!(remap.value(ir_lang::Value::from_u32(v as u32)).is_some());
            }
        }
        prop_assert!(low.module.validate().is_ok());
        let text = low.module.to_string();
        prop_assert_eq!(parse(&text).unwrap().to_string(), text);
    }
}

#[test]
fn test_round_trip_of_an_empty_module() {
    let m = Module::new("empty");
    let text = m.to_string();
    assert_eq!(text, "module \"empty\" ptr64\n");
    assert_eq!(parse(&text).unwrap().to_string(), text);
}
