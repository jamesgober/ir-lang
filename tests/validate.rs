//! Every validation rule, each with an input that breaks exactly it. Inputs are
//! written in the textual form, which (unlike the builder) accepts ill-formed IR.

use ir_lang::{
    BlockCall, Global, Linkage, Module, Overflow, Reloc, Signature, SymbolRef, Type,
    ValidationErrorKind as K, Value,
};

fn kind(text: &str) -> K {
    let m = ir_lang::parse(text).unwrap_or_else(|e| panic!("parse failed: {e}\n{text}"));
    match m.validate() {
        Ok(()) => panic!("validated:\n{text}"),
        Err(e) => {
            assert!(!e.to_string().is_empty());
            e.kind().clone()
        }
    }
}

fn ok(text: &str) {
    let m = ir_lang::parse(text).unwrap_or_else(|e| panic!("parse failed: {e}\n{text}"));
    if let Err(e) = m.validate() {
        panic!("{e}\n{text}");
    }
}

/// A module header with a few signatures: s0 = () -> (), s1 = (i64) -> (i64),
/// s2 = (ref) -> (ref), s3 = () -> (i64).
fn with(body: &str) -> String {
    format!(
        "module \"t\" ptr64\nsig s0 = () -> () fast\nsig s1 = (i64) -> (i64) fast\nsig s2 = (ref) -> (ref) fast\nsig s3 = () -> (i64) fast\n{body}"
    )
}

#[test]
fn test_well_formed_examples_validate() {
    ok(&with("func @f s1 export {\nb0(v0: i64):\n    ret v0\n}\n"));
    ok(&with(
        "func @g s0 import\nfunc @f s0 export {\nb0:\n    invoke @g() to b1 unwind b2(exn)\nb1:\n    ret\nb2(v0: ptr):\n    resume v0\n}\n",
    ));
}

// ------------------------------------------------------------------ declarations

#[test]
fn test_missing_body_and_import_with_body() {
    let mut m = Module::new("m");
    let f = m
        .declare_function("f", &Signature::new(&[], &[]), Linkage::Local)
        .unwrap();
    assert_eq!(m.validate().unwrap_err().kind(), &K::MissingBody);
    assert_eq!(m.validate_function(f).unwrap_err().kind(), &K::MissingBody);
    assert_eq!(
        kind(&with("func @f s0 import {\nb0:\n    ret\n}\n")),
        K::ImportWithBody
    );
}

#[test]
fn test_relocation_targets_must_exist() {
    let mut m = Module::new("m");
    let bad = Reloc {
        offset: 0,
        target: SymbolRef::Func(ir_lang::FuncId::from_u32(9)),
        addend: 0,
    };
    m.declare_global("g", Global::zeroed(8, 8).with_reloc(bad))
        .unwrap();
    assert!(matches!(
        m.validate().unwrap_err().kind(),
        K::UnknownFunction { .. }
    ));

    let mut m = Module::new("m");
    let bad = Reloc {
        offset: 0,
        target: SymbolRef::Global(ir_lang::GlobalId::from_u32(3)),
        addend: 0,
    };
    m.declare_global("g", Global::zeroed(8, 8).with_reloc(bad))
        .unwrap();
    assert!(matches!(
        m.validate().unwrap_err().kind(),
        K::UnknownGlobal { .. }
    ));
}

#[test]
fn test_unknown_function_handle_for_validate_function() {
    let m = Module::new("m");
    let err = m
        .validate_function(ir_lang::FuncId::from_u32(0))
        .unwrap_err();
    assert!(matches!(err.kind(), K::UnknownFunction { .. }));
}

// --------------------------------------------------------------------- structure

#[test]
fn test_entry_rules() {
    assert_eq!(kind(&with("func @f s0 export {\n}\n")), K::BadEntry);
    assert_eq!(
        kind(&with("func @f s1 export {\nb0:\n    unreachable\n}\n")),
        K::EntryParams {
            expected: 1,
            found: 0
        }
    );
    assert!(matches!(
        kind(&with(
            "func @f s1 export {\nb0(v0: i32):\n    unreachable\n}\n"
        )),
        K::TypeMismatch { .. }
    ));
    assert_eq!(
        kind(&with(
            "func @f s0 export {\nb0:\n    jump b1\nb1:\n    jump b0\n}\n"
        )),
        K::EntryBranchTarget
    );
}

#[test]
fn test_missing_terminator() {
    assert_eq!(
        kind(&with("func @f s0 export {\nb0:\n}\n")),
        K::MissingTerminator
    );
}

#[test]
fn test_edge_to_a_removed_block() {
    let mut m = Module::new("m");
    let f = m
        .declare_function("f", &Signature::new(&[], &[]), Linkage::Export)
        .unwrap();
    let mut b = m.build(f).unwrap();
    let next = b.create_block(&[]).unwrap();
    b.jump(next, &[]).unwrap();
    b.switch_to(next).unwrap();
    b.ret(&[]).unwrap();
    b.remove_block(next).unwrap();
    assert_eq!(
        m.validate().unwrap_err().kind(),
        &K::BadBlockTarget { target: next }
    );
}

#[test]
fn test_dominance_and_detached_uses() {
    // Defined in one arm, used at the join.
    let text = with(
        "func @f s1 export {\nb0(v0: i64):\n    v1: bool = const true\n    br v1, b1, b2\nb1:\n    v2: i64 = const 5\n    jump b3\nb2:\n    jump b3\nb3:\n    ret v2\n}\n",
    );
    assert_eq!(
        kind(&text),
        K::UseBeforeDef {
            value: Value::from_u32(2)
        }
    );
    // Used before its definition in the same block.
    let text = with(
        "func @f s3 export {\nb0:\n    v1: i64 = add<overflow=wrap> v0, v0\n    v0: i64 = const 1\n    ret v1\n}\n",
    );
    assert_eq!(
        kind(&text),
        K::UseBeforeDef {
            value: Value::from_u32(0)
        }
    );
    // A removed instruction's result still in use.
    let mut m = Module::new("m");
    let f = m
        .declare_function("f", &Signature::new(&[], &[Type::I32]), Linkage::Export)
        .unwrap();
    let mut b = m.build(f).unwrap();
    let c = b.iconst(Type::I32, 1).unwrap();
    b.ret(&[c]).unwrap();
    let inst = b.func().defining_inst(c).unwrap();
    b.remove_inst(inst).unwrap();
    assert_eq!(
        m.validate().unwrap_err().kind(),
        &K::UseOfDetached { value: c }
    );
}

#[test]
fn test_loop_carried_values_validate_and_unreachable_code_is_not_dominance_checked() {
    ok(&with(
        "func @f s1 export {\nb0(v0: i64):\n    jump b1(v0)\nb1(v1: i64):\n    v2: i64 = const 1\n    v3: i64 = sub<overflow=wrap> v1, v2\n    v4: i64 = const 0\n    v5: bool = gt v3, v4\n    br v5, b1(v3), b2\nb2:\n    ret v3\nb3:\n    ret v3\n}\n",
    ));
}

#[test]
fn test_bad_stack_slot() {
    assert!(matches!(
        kind(&with(
            "func @f s0 export {\n    ss0 = slot 8 align 3\nb0:\n    ret\n}\n"
        )),
        K::BadSlot { .. }
    ));
}

// ----------------------------------------------------------------------- typing

#[test]
fn test_operand_and_result_typing() {
    let f = |insts: &str| with(&format!("func @f s0 export {{\nb0:\n{insts}    ret\n}}\n"));
    assert!(matches!(
        kind(&f(
            "    v0: i32 = const 1\n    v1: i64 = const 2\n    v2: i32 = add<overflow=wrap> v0, v1\n"
        )),
        K::TypeMismatch { .. }
    ));
    assert!(matches!(
        kind(&f("    v0: bool = const true\n    v1: bool = add v0, v0\n")),
        K::BadOperandType { op: "add", .. }
    ));
    assert!(matches!(
        kind(&f(
            "    v0: i32 = const 1\n    v1: i64 = add<overflow=wrap> v0, v0\n"
        )),
        K::ResultMismatch
    ));
    assert!(matches!(
        kind(&f("    v0: u8 = const 0x1ff\n")),
        K::BadConst {
            ty: Type::U8,
            bits: 0x1ff
        }
    ));
    assert!(matches!(
        kind(&f("    v0: ptr = const 0x10\n")),
        K::BadConst { ty: Type::Ptr, .. }
    ));
    assert!(matches!(
        kind(&f(
            "    v0: f64 = const 1.0\n    v1: f64 = floor_div v0, v0\n"
        )),
        K::BadOperandType { .. }
    ));
    assert!(matches!(
        kind(&f(
            "    v0: i32 = const 1\n    v1: bool = lt v0, v0\n    v2: bool = lt v1, v1\n"
        )),
        K::BadOperandType { op: "lt", .. }
    ));
}

#[test]
fn test_policies_must_be_exact() {
    let f = |insts: &str| with(&format!("func @f s0 export {{\nb0:\n{insts}    ret\n}}\n"));
    // Missing overflow.
    assert!(matches!(
        kind(&f("    v0: i32 = const 1\n    v1: i32 = add v0, v0\n")),
        K::PolicyMismatch { op: "add", .. }
    ));
    // Extra field on a float op.
    assert!(matches!(
        kind(&f(
            "    v0: f64 = const 1.0\n    v1: f64 = add<overflow=wrap> v0, v0\n"
        )),
        K::PolicyMismatch { .. }
    ));
    // div needs both fields.
    assert!(matches!(
        kind(&f(
            "    v0: i32 = const 1\n    v1: i32 = div<overflow=wrap> v0, v0\n"
        )),
        K::PolicyMismatch { op: "div", .. }
    ));
    // An error policy belongs in a check.
    assert!(matches!(
        kind(&f(
            "    v0: i32 = const 1\n    v1: i32 = add<overflow=error> v0, v0\n"
        )),
        K::ErrorOutsideCheck { op: "add" }
    ));
    assert!(matches!(
        kind(&f(
            "    v0: u32 = const 1\n    v1: u32 = char_from_u32 v0\n"
        )),
        K::ErrorOutsideCheck {
            op: "char_from_u32"
        }
    ));
}

#[test]
fn test_check_terminator_rules() {
    let text = |op: &str| {
        with(&format!(
            "func @f s0 export {{\nb0:\n    v0: i32 = const 1\n    check {op} to b1(res0) error b2(err)\nb1(v1: i32):\n    ret\nb2(v2: u32):\n    ret\n}}\n"
        ))
    };
    ok(&text("add<overflow=error> v0, v0"));
    assert!(matches!(
        kind(&text("add<overflow=wrap> v0, v0")),
        K::NotFallible { .. }
    ));
    assert!(matches!(kind(&text("eq v0, v0")), K::NotFallible { .. }));
    // The error edge carries a u32 code, the normal edge the result.
    let wrong = with(
        "func @f s0 export {\nb0:\n    v0: i32 = const 1\n    check add<overflow=error> v0, v0 to b1(err) error b2(res0)\nb1(v1: i32):\n    ret\nb2(v2: u32):\n    ret\n}\n",
    );
    assert_eq!(kind(&wrong), K::BadEdgeArg);
}

#[test]
fn test_conversions_are_checked() {
    let f = |insts: &str| with(&format!("func @f s0 export {{\nb0:\n{insts}    ret\n}}\n"));
    for bad in [
        "    v0: i32 = const 1\n    v1: i16 = sext v0\n",
        "    v0: i32 = const 1\n    v1: i64 = narrow v0\n",
        "    v0: i32 = const 1\n    v1: i64 = bitcast v0\n",
        "    v0: i32 = const 1\n    v1: ptr = int_to_ptr v0\n",
        "    v0: f32 = const 1.0\n    v1: f32 = float_cast v0\n",
        "    v0: i32 = const 1\n    v1: i32 = int_cast<overflow=wrap> v0\n",
    ] {
        assert!(matches!(kind(&f(bad)), K::BadConversion { .. }), "{bad}");
    }
    ok(&f(
        "    v0: f32 = const 1.0\n    v1: u32 = bitcast v0\n    v2: i64 = const 1\n    v3: ptr = int_to_ptr v2\n",
    ));
}

#[test]
fn test_memory_rules() {
    let f = |insts: &str| {
        with(&format!(
            "type t0 = struct {{ i32, i64 }}\nfunc @f s0 export {{\n    ss0 = slot 16 align 8\nb0:\n    v0: ptr = stack_addr ss0\n{insts}    ret\n}}\n"
        ))
    };
    assert!(matches!(
        kind(&f("    v1: i32 = load v0 align 3\n")),
        K::BadAlignment { align: 3 }
    ));
    assert!(matches!(
        kind(&f("    v1: ptr = field_addr v0, t0, 2\n")),
        K::BadField { field: 2, .. }
    ));
    assert!(matches!(
        kind(&f("    v1: ptr = field_addr v0, t7, 0\n")),
        K::UnknownAggregate { .. }
    ));
    assert!(matches!(
        kind(&f("    v1: i32 = atomic_load v0 release\n")),
        K::BadOrdering
    ));
    assert!(matches!(
        kind(&f(
            "    v1: i32 = const 1\n    atomic_store v1, v0 acquire\n"
        )),
        K::BadOrdering
    ));
    assert!(matches!(kind(&f("    fence relaxed\n")), K::BadOrdering));
    assert!(matches!(
        kind(&f(
            "    v1: i32 = const 1\n    v2: i32, v3: bool = cmpxchg v0, v1, v1 relaxed seq_cst\n"
        )),
        K::BadOrdering
    ));
    assert!(matches!(
        kind(&f("    v1: f64 = atomic_load v0 seq_cst\n")),
        K::BadOperandType { .. }
    ));
    assert!(matches!(
        kind(&f("    v1: i64 = const 8\n    memcpy v0, v0, v1 align 1\n")),
        K::BadOperandType { op: "memcpy", .. }
    ));
    assert!(matches!(
        kind(&f("    v1: ptr = stack_addr ss4\n")),
        K::BadSlot { .. }
    ));
    ok(&f(
        "    v1: i64 = const 1\n    v2: ptr = elem_addr v0, t0, v1\n    v3: u64 = const 4\n    v4: u8 = const 0\n    memset v2, v4, v3 align 4 volatile\n",
    ));
}

#[test]
fn test_call_and_return_rules() {
    assert!(matches!(
        kind(&with(
            "func @g s1 import\nfunc @f s0 export {\nb0:\n    v0: i64 = call @g()\n    ret\n}\n"
        )),
        K::ArgCount {
            expected: 1,
            found: 0
        }
    ));
    assert!(matches!(
        kind(&with("func @f s3 export {\nb0:\n    ret\n}\n")),
        K::ArgCount {
            expected: 1,
            found: 0
        }
    ));
    assert!(matches!(
        kind(&with(
            "func @f s1 export {\nb0(v0: i64):\n    v1: ptr = func_addr @f\n    v2: i64 = call_indirect s1 v0(v0)\n    ret v2\n}\n"
        )),
        K::TypeMismatch { .. }
    ));
    // Tail calls need equal returns.
    assert_eq!(
        kind(&with(
            "func @g s0 import\nfunc @f s3 export {\nb0:\n    tail_call @g()\n}\n"
        )),
        K::BadTailCall
    );
    ok(&with(
        "func @g s3 import\nfunc @f s3 export {\nb0:\n    tail_call @g()\n}\n",
    ));
}

#[test]
fn test_edge_rules() {
    assert!(matches!(
        kind(&with(
            "func @f s1 export {\nb0(v0: i64):\n    jump b1\nb1(v1: i64):\n    ret v1\n}\n"
        )),
        K::EdgeArgCount {
            expected: 1,
            found: 0,
            ..
        }
    ));
    assert!(matches!(
        kind(&with(
            "func @f s0 export {\nb0:\n    v0: i32 = const 1\n    switch v0 default b1 [1: b1, 1: b1]\nb1:\n    ret\n}\n"
        )),
        K::BadSwitchCase { value: 1 }
    ));
    assert!(matches!(
        kind(&with(
            "func @f s0 export {\nb0:\n    v0: u8 = const 1\n    switch v0 default b1 [256: b1]\nb1:\n    ret\n}\n"
        )),
        K::BadSwitchCase { value: 256 }
    ));
    // `exn` on a plain jump.
    assert_eq!(
        kind(&with(
            "func @f s0 export {\nb0:\n    jump b1(exn)\nb1(v0: ptr):\n    ret\n}\n"
        )),
        K::BadEdgeArg
    );
}

#[test]
fn test_exception_edge_rules() {
    // A landing pad also reached by a jump.
    let text = with(
        "func @g s0 import\nfunc @f s0 export {\nb0:\n    v0: bool = const true\n    br v0, b1, b3\nb1:\n    invoke @g() to b2 unwind b3\nb2:\n    ret\nb3:\n    ret\n}\n",
    );
    assert!(matches!(kind(&text), K::LandingPadPredecessor { .. }));
    // Normal and unwind edges to the same block.
    let text = with(
        "func @g s0 import\nfunc @f s0 export {\nb0:\n    invoke @g() to b1 unwind b1\nb1:\n    ret\n}\n",
    );
    assert!(matches!(kind(&text), K::LandingPadPredecessor { .. }));
    // A returned value is not available on the unwind edge.
    let text = with(
        "func @g s3 import\nfunc @f s0 export {\nb0:\n    invoke @g() to b1(res0) unwind b2(res0)\nb1(v0: i64):\n    ret\nb2(v1: i64):\n    ret\n}\n",
    );
    assert_eq!(kind(&text), K::BadEdgeArg);
    // `nounwind` functions do not unwind.
    let text =
        with("func @g s0 import\nfunc @f s0 export [nounwind] {\nb0:\n    call @g()\n    ret\n}\n");
    assert_eq!(kind(&text), K::UnwindInNounwind);
    ok(&with(
        "func @g s0 import [nounwind]\nfunc @f s0 export [nounwind] {\nb0:\n    call @g()\n    ret\n}\n",
    ));
    ok(&with(
        "func @g s0 import\nfunc @f s0 export [nounwind] {\nb0:\n    call @g() [nounwind]\n    ret\n}\n",
    ));
    let text =
        with("func @f s0 export [nounwind] {\nb0:\n    v0: ptr = const null\n    resume v0\n}\n");
    assert_eq!(kind(&text), K::UnwindInNounwind);
    // `noreturn` functions do not return.
    assert_eq!(
        kind(&with("func @f s0 export [noreturn] {\nb0:\n    ret\n}\n")),
        K::ReturnInNoreturn
    );
    ok(&with(
        "func @f s0 export [noreturn] {\nb0:\n    trap 9\n}\n",
    ));
}

// --------------------------------------------------------------------- safepoints

#[test]
fn test_refs_must_be_relocated_across_safepoints() {
    // `v0` is used after the call without relocation.
    let text = with(
        "func @g s0 import\nfunc @f s2 export {\nb0(v0: ref):\n    call @g()\n    ret v0\n}\n",
    );
    assert_eq!(
        kind(&text),
        K::LiveAcrossSafepoint {
            value: Value::from_u32(0)
        }
    );
    // Relocated through the GC list: fine.
    ok(&with(
        "func @g s0 import\nfunc @f s2 export {\nb0(v0: ref):\n    v1: ref = call @g() gc(v0)\n    ret v1\n}\n",
    ));
    // Live across a safepoint into another block.
    let text = with(
        "func @g s0 import\nfunc @f s2 export {\nb0(v0: ref):\n    safepoint gc()\n    jump b1\nb1:\n    ret v0\n}\n",
    );
    assert!(matches!(kind(&text), K::LiveAcrossSafepoint { .. }));
    // Live around a loop whose body has a safepoint.
    let text = with(
        "func @g s0 import\nfunc @f s2 export {\nb0(v0: ref):\n    jump b1\nb1:\n    call @g()\n    v1: bool = const false\n    br v1, b1, b2\nb2:\n    ret v0\n}\n",
    );
    assert!(matches!(kind(&text), K::LiveAcrossSafepoint { .. }));
    // An invoke's edge arguments are read after the call.
    let text = with(
        "func @g s0 import\nfunc @f s2 export {\nb0(v0: ref):\n    invoke @g() to b1(v0) unwind b2(exn)\nb1(v1: ref):\n    ret v1\nb2(v2: ptr):\n    resume v2\n}\n",
    );
    assert!(matches!(kind(&text), K::LiveAcrossSafepoint { .. }));
    ok(&with(
        "func @g s0 import\nfunc @f s2 export {\nb0(v0: ref):\n    invoke @g() gc(v0) to b1(res0) unwind b2(exn)\nb1(v1: ref):\n    ret v1\nb2(v2: ptr):\n    resume v2\n}\n",
    ));
    // `nogc` calls are not safepoints, and cannot carry a GC list.
    ok(&with(
        "func @g s0 import [nogc]\nfunc @f s2 export {\nb0(v0: ref):\n    call @g()\n    ret v0\n}\n",
    ));
    assert_eq!(
        kind(&with(
            "func @g s0 import [nogc]\nfunc @f s2 export {\nb0(v0: ref):\n    v1: ref = call @g() gc(v0)\n    ret v1\n}\n"
        )),
        K::BadGcList
    );
    assert_eq!(
        kind(&with(
            "func @f s0 export [nogc] {\nb0:\n    safepoint gc()\n    ret\n}\n"
        )),
        K::SafepointInNogc
    );
    assert_eq!(
        kind(&with(
            "func @f s0 export {\nb0:\n    v0: i64 = const 1\n    v1: ref = safepoint gc(v0)\n    ret\n}\n"
        )),
        K::BadGcList
    );
}

#[test]
fn test_derived_pointers_never_cross_safepoints_or_escape() {
    let text = with(
        "func @g s0 import\nfunc @f s2 export {\nb0(v0: ref):\n    v1: ptr = ref_to_ptr v0\n    v2: i64 = const 8\n    v3: ptr = ptr_offset v1, v2\n    v4: ref = call @g() gc(v0)\n    v5: i64 = load v3 align 8\n    ret v4\n}\n",
    );
    assert_eq!(
        kind(&text),
        K::LiveAcrossSafepoint {
            value: Value::from_u32(3)
        }
    );
    let text = with(
        "func @f s2 export {\n    ss0 = slot 8 align 8\nb0(v0: ref):\n    v1: ptr = ref_to_ptr v0\n    v2: ptr = stack_addr ss0\n    store v1, v2 align 8\n    ret v0\n}\n",
    );
    assert_eq!(
        kind(&text),
        K::DerivedPointerEscapes {
            value: Value::from_u32(1)
        }
    );
    // Recomputing the derived pointer after the safepoint is the valid form.
    ok(&with(
        "func @g s0 import\nfunc @f s2 export {\nb0(v0: ref):\n    v1: ref = call @g() gc(v0)\n    v2: ptr = ref_to_ptr v1\n    v3: i64 = load v2 align 8\n    ret v1\n}\n",
    ));
}

#[test]
fn test_builder_and_validator_agree_on_a_wrong_edge() {
    // The builder refuses an edge that the validator would refuse.
    let mut m = Module::new("m");
    let f = m
        .declare_function("f", &Signature::new(&[], &[]), Linkage::Export)
        .unwrap();
    let mut b = m.build(f).unwrap();
    let next = b.create_block(&[Type::I32]).unwrap();
    assert!(b.jump(next, &[]).is_err());
    let x = b.iconst(Type::I64, 1).unwrap();
    assert!(b.jump(next, &[x]).is_err());
    let entry = b.func().entry();
    assert!(
        b.set_terminator(ir_lang::Terminator::Jump(BlockCall::new(entry, &[])))
            .is_err()
    );
    let y = b.iconst(Type::I32, 1).unwrap();
    assert!(b.add(y, y, Overflow::Error).is_err());
    b.jump(next, &[y]).unwrap();
    b.switch_to(next).unwrap();
    b.ret(&[]).unwrap();
    m.validate().unwrap();
}
