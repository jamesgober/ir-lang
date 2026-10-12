//! The textual form: every instruction and terminator round-trips through
//! print and parse; parse errors are positioned; budgets hold.

use ir_lang::{ParseErrorKind, ParseLimits, parse, parse_with};

/// Every declaration form, every instruction, and every terminator, in canonical
/// printer output (so `print(parse(SINK)) == SINK`). It also validates.
const SINK: &str = r#"module "sink" ptr64
type t0 = struct { u8, i64 }
type t1 = packed struct { u8, u32 }
type t2 = [t0 x 4]
type t3 = struct { }
sig s0 = () -> () fast
sig s1 = (i64, f64) -> (i64) c
sig s2 = (ref) -> (ref) fast
sig s3 = () -> (i64) fast
global @data export const size 6 align 1 bytes "68656c6c6f00"
global @table local mutable size 16 align 8 zeroed reloc 0 @callee +0 reloc 8 @data +2
global @ext import const size 8 align 8 zeroed
global @"odd name" export const size 1 align 1 zeroed

func @callee s1 local [inline] {
b0(v0: i64, v1: f64):
    ret v0
}
func @ext_fn s0 import [nounwind, nogc]
func @thrower s3 import

func @sink s1 export {
    ss0 = slot 64 align 16
    ss1 = slot 0 align 1
b0(v0: i64, v1: f64):
    v2: i64 = const -5
    v3: u8 = const 255
    v4: f64 = const -0.0
    v5: f32 = const nan:0x7fc00001
    v6: bool = const true
    v7: ptr = const null
    v8: ref = const null
    v9: f64 = const inf
    v10: f64 = const -inf
    v11: i64 = neg<overflow=wrap> v2
    v12: f64 = sqrt v1
    v13: i64 = div<overflow=wrap,div_zero=trap> v0, v2
    v14: i64 = shl<shift=mask> v0, v2
    v15: f64 = fma v1, v1, v4
    v16: bool = le v0, v2
    v17: i8 = total_cmp v1, v4
    v18: i32 = int_cast<overflow=wrap> v0
    v19: u64 = zext v3
    v20: i64 = float_to_int<float_to_int=saturate> v1
    v21: u64 = bitcast v1
    v22: i64 = select v16, v0, v2
    v23: ptr = stack_addr ss0
    v24: ptr = global_addr @data
    v25: ptr = func_addr @callee
    v26: ptr = field_addr v23, t0, 1
    v27: ptr = elem_addr v23, t0, v2
    v28: ptr = ptr_offset v23, v0
    v29: i64 = load v26 align 8 volatile
    store v29, v26 align 8
    v30: i64 = atomic_load v26 seq_cst
    atomic_store v30, v26 release
    v31: i64 = atomic_rmw add v26, v30 acq_rel
    v32: i64, v33: bool = cmpxchg v26, v30, v31 seq_cst relaxed
    fence acquire
    v34: u64 = const 16
    memcpy v23, v24, v34 align 1
    memmove v23, v28, v34 align 1 volatile
    memset v23, v3, v34 align 16
    v35: i64 = call @callee(v0, v1) [cold]
    v36: i64 = call_indirect s1 v25(v0, v1)
    call @ext_fn()
    safepoint gc()
    v37: u32 = const 9
    v39: f64 = ieee_rem v1, v4
    v49: f32 = float_cast v1
    v50: ptr = global_addr @"odd name"
    v51: ptr = global_addr @table
    v52: ptr = global_addr @ext
    v53: u64 = pow<overflow=wrap> v19, v34
    v54: f64 = pow v1, v4
    v55: u64 = shr<shift=saturate> v19, v34
    switch v37 default b1 [0: b1, 7: b2(v37)]
b1:
    br v16, b3, b4
b2(v38: u32):
    check char_from_u32 v38 as u32 to b5(res0) error b6(err)
b3:
    invoke @thrower() to b7(res0) unwind b8(exn)
b4:
    invoke_indirect s1 v25(v0, v1) to b9(res0) unwind b10(exn)
b5(v40: u32):
    check add<overflow=error> v0, v2 to b11(res0) error b6(err)
b6(v41: u32):
    trap 3
b7(v42: i64):
    tail_call @callee(v42, v1)
b8(v43: ptr):
    resume v43
b9(v44: i64):
    tail_call_indirect s1 v25(v44, v1)
b10(v45: ptr):
    unreachable
b11(v46: i64):
    jump b13(v46)
b12(v47: i64):
    ret v47
b13(v48: i64):
    check neg<overflow=error> v48 to b12(res0) error b6(err)
}

func @gc s2 export {
b0(v0: ref):
    v1: ref = safepoint gc(v0)
    v2: ptr = ref_to_ptr v1
    v3: i64 = load v2 align 8
    v4: ref, v5: ref = call @gc(v1) gc(v1)
    v6: bool = eq v4, v5
    ret v4
}
"#;

#[test]
fn test_every_form_round_trips_and_validates() {
    let m = parse(SINK).unwrap_or_else(|e| panic!("{e}"));
    if let Err(e) = m.validate() {
        panic!("{e}");
    }
    assert_eq!(m.to_string(), SINK);
    assert_eq!(m.function_count(), 5);
    assert_eq!(m.global_count(), 4);
}

#[test]
fn test_32_bit_module_round_trips() {
    let text = "module \"w\" ptr32\nsig s0 = (u32) -> (ptr) fast\n\nfunc @f s0 export {\nb0(v0: u32):\n    v1: ptr = int_to_ptr v0\n    ret v1\n}\n";
    let m = parse(text).unwrap();
    m.validate().unwrap();
    assert_eq!(m.to_string(), text);
    // A 64-bit integer is not an address in a 32-bit module.
    let bad = text.replace("(u32)", "(u64)").replace("v0: u32", "v0: u64");
    assert!(parse(&bad).unwrap().validate().is_err());
}

#[test]
fn test_comments_and_crlf_are_accepted() {
    let text = "; a comment\r\nmodule \"m\" ptr64 ; trailing\r\nsig s0 = () -> () fast\r\nfunc @f s0 export {\r\nb0:\r\n    ret ; done\r\n}\r\n";
    let m = parse(text).unwrap();
    m.validate().unwrap();
}

fn err(text: &str) -> (u32, u32, ParseErrorKind) {
    let e = parse(text).unwrap_err();
    assert!(!e.to_string().is_empty());
    (e.line(), e.column(), e.kind().clone())
}

#[test]
fn test_errors_are_positioned_and_specific() {
    let head = "module \"m\" ptr64\nsig s0 = () -> () fast\n";
    let (line, col, k) = err(&format!(
        "{head}func @f s0 export {{\nb0:\n    v0: i32 = frob v1\n}}\n"
    ));
    assert_eq!((line, col), (5, 15));
    assert_eq!(
        k,
        ParseErrorKind::UnknownOpcode {
            name: "frob".into()
        }
    );

    let (_, _, k) = err(&format!(
        "{head}func @f s0 export {{\nb0:\n    v0: int = const 1\n}}\n"
    ));
    assert_eq!(k, ParseErrorKind::UnknownType { name: "int".into() });

    let (_, _, k) = err(&format!(
        "{head}func @f s0 export {{\nb0:\n    ret v9\n}}\n"
    ));
    assert_eq!(k, ParseErrorKind::Undefined { what: "v9".into() });

    let (_, _, k) = err(&format!(
        "{head}func @f s0 export {{\nb0:\n    jump b4\n}}\n"
    ));
    assert_eq!(k, ParseErrorKind::Undefined { what: "b4".into() });

    let (_, _, k) = err(&format!(
        "{head}func @f s0 export {{\nb0:\n    v0: i32 = const 1\n    v0: i32 = const 2\n    ret\n}}\n"
    ));
    assert_eq!(k, ParseErrorKind::Duplicate { what: "v0".into() });

    let (_, _, k) = err(&format!(
        "{head}func @f s0 export {{\nb0:\n    v0: i32, v2: bool = cmpxchg v0, v0, v0 seq_cst seq_cst\n}}\n"
    ));
    assert_eq!(k, ParseErrorKind::NonConsecutiveResults);

    let (_, _, k) = err(&format!(
        "{head}func @f s0 export {{\nb0:\n    call @nowhere()\n}}\n"
    ));
    assert_eq!(
        k,
        ParseErrorKind::UnknownSymbol {
            name: "nowhere".into()
        }
    );

    let (_, _, k) = err(&format!(
        "{head}func @f s0 export {{\nb0:\n    v0: u8 = const 256\n}}\n"
    ));
    assert_eq!(k, ParseErrorKind::BadNumber { text: "256".into() });

    let (_, _, k) = err(&format!(
        "{head}func @f s0 export {{\nb0:\n    v0: i32 = add<overflow=promote> v0, v0\n}}\n"
    ));
    assert!(
        matches!(
            k,
            ParseErrorKind::Expected {
                expected: "a policy value",
                ..
            }
        ),
        "promote is not representable"
    );

    let (_, _, k) = err(&format!(
        "{head}func @f s0 export {{\nb0:\n    load v0 align 8\n}}\n"
    ));
    assert_eq!(k, ParseErrorKind::MissingResult);

    let (_, _, k) = err("module \"m\" ptr64\nsig s1 = () -> () fast\n");
    assert_eq!(k, ParseErrorKind::OutOfOrder { what: "signatures" });

    let (_, _, k) = err("module \"m\" ptr64\nsig s0 = () -> () fast\nsig s1 = () -> () fast\n");
    assert_eq!(
        k,
        ParseErrorKind::Duplicate {
            what: "signature".into()
        }
    );

    let (_, _, k) = err(
        "module \"m\" ptr64\nglobal @g export const size 8 align 8 zeroed\nglobal @g export const size 8 align 8 zeroed\n",
    );
    assert!(matches!(k, ParseErrorKind::Module(_)));

    let (_, _, k) = err(&format!(
        "{head}func @f s0 export {{
b0:
    ret
    v0: i32 = const 1
}}
"
    ));
    assert!(
        matches!(k, ParseErrorKind::Expected { .. }),
        "nothing may follow a terminator"
    );

    let (_, _, k) = err("module \"m\" ptr64 ~");
    assert_eq!(k, ParseErrorKind::UnexpectedChar { found: '~' });

    let (_, _, k) = err("module \"m");
    assert_eq!(k, ParseErrorKind::UnterminatedString);

    let (_, _, k) = err("module \"m\" ptr16");
    assert!(matches!(k, ParseErrorKind::Expected { .. }));
}

#[test]
fn test_budgets_hold() {
    // Input size.
    let limits = ParseLimits {
        max_bytes: 10,
        ..ParseLimits::default()
    };
    assert!(matches!(
        parse_with("module \"m\" ptr64\n", &limits)
            .unwrap_err()
            .kind(),
        ParseErrorKind::InputTooLarge { limit: 10 }
    ));
    // A huge id in a tiny input is refused before any table is allocated.
    let text = "module \"m\" ptr64\nsig s0 = () -> () fast\nfunc @f s0 export {\nb0:\n    v4000000000: i32 = const 1\n}\n";
    assert!(matches!(
        parse(text).unwrap_err().kind(),
        ParseErrorKind::IdTooLarge { .. }
    ));
    let text =
        "module \"m\" ptr64\nsig s0 = () -> () fast\nfunc @f s0 export {\nb99999999:\n    ret\n}\n";
    assert!(matches!(
        parse(text).unwrap_err().kind(),
        ParseErrorKind::IdTooLarge { .. }
    ));
    // The explicit cap applies too.
    let limits = ParseLimits {
        max_id: 3,
        ..ParseLimits::default()
    };
    let text = "module \"m\" ptr64\nsig s0 = () -> () fast\nfunc @f s0 export {\nb0:\n    v4: i32 = const 1\n    ret\n}\n";
    assert!(matches!(
        parse_with(text, &limits).unwrap_err().kind(),
        ParseErrorKind::IdTooLarge { id: 4 }
    ));
}

#[test]
fn test_deeply_repetitive_input_parses_in_linear_time() {
    // 200k instructions in one block: the parser has no recursion and no
    // quadratic step.
    let mut text =
        String::from("module \"m\" ptr64\nsig s0 = () -> () fast\n\nfunc @f s0 export {\nb0:\n");
    for i in 0..200_000 {
        text.push_str(&format!("    v{i}: i64 = const {i}\n"));
    }
    text.push_str("    ret\n}\n");
    let m = parse(&text).unwrap();
    m.validate().unwrap();
    assert_eq!(m.to_string(), text);
}

#[test]
fn test_symbol_quoting_round_trips() {
    let text = "module \"we\\\"ird\\n\" ptr64\nsig s0 = () -> () fast\nglobal @\"has space\" export const size 1 align 1 zeroed\nglobal @\"\\u{1}ctl\" export const size 1 align 1 zeroed\nfunc @\"9lives\" s0 import\n";
    let m = parse(text).unwrap();
    assert_eq!(m.name(), "we\"ird\n");
    assert_eq!(
        m.global_name(ir_lang::GlobalId::from_u32(1)),
        Some("\u{1}ctl")
    );
    assert_eq!(m.to_string(), text);
}
