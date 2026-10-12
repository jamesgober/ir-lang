//! OPS v2 in ir-lang: integer and float `pow`, `shift = saturate`, and the
//! `NegativeExponent` (E0006) error kind. The interpreter is checked against
//! independent references over every integer type, policy, and the OPS edge
//! values; the validator's placement rules (which operations need a `check`);
//! and the textual form.

// The reference helpers serve the interpreter checks.
#![cfg_attr(not(feature = "interp"), allow(dead_code))]

use ir_lang::{
    BinaryOp, BlockArg, BlockCall, BuildError, InstData, Linkage, Module, OpError, Overflow,
    Policy, Shift, Signature, Type, ValidationErrorKind,
};

const INT_TYPES: [Type; 8] = [
    Type::I8,
    Type::I16,
    Type::I32,
    Type::I64,
    Type::U8,
    Type::U16,
    Type::U32,
    Type::U64,
];

fn width(ty: Type) -> u32 {
    ty.int_bits().unwrap()
}

fn range(ty: Type) -> (i128, i128) {
    let w = width(ty);
    if ty.is_signed_int() {
        (-(1i128 << (w - 1)), (1i128 << (w - 1)) - 1)
    } else {
        (0, (1i128 << w) - 1)
    }
}

fn to_bits(ty: Type, v: i128) -> u64 {
    let w = width(ty);
    let m: u128 = if w == 64 {
        u64::MAX as u128
    } else {
        (1u128 << w) - 1
    };
    ((v as u128) & m) as u64
}

/// The OPS edge values for an integer type, plus small exponents and shift
/// amounts around the width.
fn edges(ty: Type) -> Vec<i128> {
    let (lo, hi) = range(ty);
    let w = i128::from(width(ty));
    let mut v = vec![
        0,
        1,
        -1,
        lo,
        hi,
        lo + 1,
        hi - 1,
        2,
        -2,
        3,
        -3,
        5,
        7,
        -8,
        w - 1,
        w,
        w + 1,
        62,
        63,
        64,
        65,
        127,
        128,
        hi / 3,
    ];
    v.retain(|x| *x >= lo && *x <= hi);
    v.sort_unstable();
    v.dedup();
    v
}

/// The exact power `a ** e` (e >= 0) if it is at most `limit` in magnitude, by
/// plain multiplication (so independent of the interpreter's squaring).
fn exact_pow(a: i128, e: i128, limit: i128) -> Option<i128> {
    if a == 0 {
        return Some(i128::from(e == 0));
    }
    if a == 1 {
        return Some(1);
    }
    if a == -1 {
        return Some(if e % 2 == 0 { 1 } else { -1 });
    }
    let mut acc: i128 = 1;
    let mut k = 0;
    while k < e {
        acc = acc.checked_mul(a)?;
        if acc.abs() > limit {
            return None;
        }
        k += 1;
    }
    Some(acc)
}

/// The low `width` bits of `a ** e`, most significant exponent bit first.
fn wrapped_pow(ty: Type, a: i128, e: i128) -> u64 {
    let m: u128 = if width(ty) == 64 {
        u64::MAX as u128
    } else {
        (1u128 << width(ty)) - 1
    };
    let base = (a as u128) & m;
    let mut acc: u128 = 1;
    for bit in (0..128).rev() {
        acc = acc.wrapping_mul(acc) & m;
        if ((e as u128) >> bit) & 1 == 1 {
            acc = acc.wrapping_mul(base) & m;
        }
    }
    acc as u64
}

#[cfg(feature = "interp")]
mod semantics {
    use super::*;
    use ir_lang::interp::{OpFault, eval_binary};

    #[test]
    fn test_integer_pow_matches_reference_under_every_policy() {
        let mut checked = 0u32;
        for ty in INT_TYPES {
            let (lo, hi) = range(ty);
            for ov in [Overflow::Error, Overflow::Wrap, Overflow::Trap] {
                let p = Policy::overflow(ov);
                for a in edges(ty) {
                    for e in edges(ty) {
                        let got = eval_binary(BinaryOp::Pow, p, ty, to_bits(ty, a), to_bits(ty, e));
                        // OPS §3 `pow`, §6 E0006: a negative exponent is the
                        // error under every policy, `trap` included.
                        let want = if e < 0 {
                            Err(OpFault::Error(OpError::NegativeExponent))
                        } else {
                            match exact_pow(a, e, hi.max(-lo)) {
                                Some(v) if v >= lo && v <= hi => Ok(to_bits(ty, v)),
                                _ => match ov {
                                    Overflow::Wrap => Ok(wrapped_pow(ty, a, e)),
                                    Overflow::Trap => Err(OpFault::Trap(OpError::ArithOverflow)),
                                    _ => Err(OpFault::Error(OpError::ArithOverflow)),
                                },
                            }
                        };
                        assert_eq!(got, want, "{ty} {a} ** {e} {p}");
                        checked += 1;
                    }
                }
            }
        }
        assert!(checked > 5_000);
    }

    #[test]
    fn test_integer_pow_boundaries() {
        let wrap = Policy::overflow(Overflow::Wrap);
        let err = Policy::overflow(Overflow::Error);
        // 0 ** 0 = 1; MIN reachable exactly; one past overflows.
        assert_eq!(eval_binary(BinaryOp::Pow, err, Type::I32, 0, 0), Ok(1));
        assert_eq!(
            eval_binary(BinaryOp::Pow, err, Type::I64, to_bits(Type::I64, -2), 63),
            Ok(i64::MIN as u64)
        );
        assert_eq!(
            eval_binary(BinaryOp::Pow, err, Type::I64, 2, 63),
            Err(OpFault::Error(OpError::ArithOverflow))
        );
        assert_eq!(
            eval_binary(BinaryOp::Pow, err, Type::U64, 2, 63),
            Ok(1 << 63)
        );
        assert_eq!(
            eval_binary(BinaryOp::Pow, err, Type::U64, 2, 64),
            Err(OpFault::Error(OpError::ArithOverflow))
        );
        // Huge exponents: wrap keeps the low bits; an odd base never reaches 0.
        assert_eq!(
            eval_binary(BinaryOp::Pow, wrap, Type::U64, 2, u64::MAX),
            Ok(0)
        );
        assert_eq!(
            eval_binary(BinaryOp::Pow, wrap, Type::U64, 3, u64::MAX),
            Ok(wrapped_pow(Type::U64, 3, u64::MAX as i128))
        );
        assert_eq!(
            eval_binary(BinaryOp::Pow, wrap, Type::I8, to_bits(Type::I8, -1), 127),
            Ok(0xff)
        );
    }

    /// The reference for `shift = saturate` (OPS v2, PHP semantics).
    fn saturate_ref(op: BinaryOp, ty: Type, a: i128, n: i128) -> Result<u64, OpFault> {
        let w = i128::from(width(ty));
        if n < 0 {
            return Err(OpFault::Error(OpError::ShiftOutOfRange));
        }
        if n >= w {
            return Ok(if op == BinaryOp::Shr && a < 0 {
                to_bits(ty, -1)
            } else {
                0
            });
        }
        Ok(if op == BinaryOp::Shl {
            to_bits(ty, a << n)
        } else {
            to_bits(ty, a.div_euclid(1i128 << n))
        })
    }

    #[test]
    fn test_shift_saturate_matches_reference() {
        let p = Policy::shift(Shift::Saturate);
        for ty in INT_TYPES {
            for op in [BinaryOp::Shl, BinaryOp::Shr] {
                for a in edges(ty) {
                    for n in edges(ty) {
                        let got = eval_binary(op, p, ty, to_bits(ty, a), to_bits(ty, n));
                        assert_eq!(got, saturate_ref(op, ty, a, n), "{op:?} {ty} {a} {n}");
                    }
                }
            }
        }
    }

    #[test]
    fn test_shift_saturate_vs_mask_and_error_on_the_width() {
        let ty = Type::I32;
        let neg = to_bits(ty, -8);
        let sat = Policy::shift(Shift::Saturate);
        let mask = Policy::shift(Shift::Mask);
        assert_eq!(
            eval_binary(BinaryOp::Shr, sat, ty, neg, 32),
            Ok(0xffff_ffff)
        );
        assert_eq!(eval_binary(BinaryOp::Shr, mask, ty, neg, 32), Ok(neg));
        assert_eq!(eval_binary(BinaryOp::Shl, sat, ty, 1, 40), Ok(0));
        assert_eq!(
            eval_binary(BinaryOp::Shl, Policy::shift(Shift::Error), ty, 1, 40),
            Err(OpFault::Error(OpError::ShiftOutOfRange))
        );
        assert_eq!(eval_binary(BinaryOp::Shr, sat, Type::U8, 0x80, 255), Ok(0));
    }

    #[test]
    fn test_float_pow_follows_annex_f_at_both_widths() {
        let f = |x: f64, y: f64| {
            f64::from_bits(
                eval_binary(
                    BinaryOp::Pow,
                    Policy::NONE,
                    Type::F64,
                    x.to_bits(),
                    y.to_bits(),
                )
                .unwrap(),
            )
        };
        assert_eq!(f(f64::NAN, 0.0), 1.0);
        assert_eq!(f(1.0, f64::NAN), 1.0);
        assert_eq!(f(2.0, 0.5), std::f64::consts::SQRT_2);
        assert!(f(-8.0, 1.0 / 3.0).is_nan());
        let g = |x: f32, y: f32| {
            f32::from_bits(
                eval_binary(
                    BinaryOp::Pow,
                    Policy::NONE,
                    Type::F32,
                    u64::from(x.to_bits()),
                    u64::from(y.to_bits()),
                )
                .unwrap() as u32,
            )
        };
        assert_eq!(g(2.0, 10.0), 1024.0);
        assert_eq!(g(-0.0, 3.0).to_bits(), (-0.0f32).to_bits());
        assert_eq!(g(f32::MAX, 2.0), f32::INFINITY);
    }

    /// `check pow` routes E0006 to the error edge under `wrap` and under `trap`
    /// (where only an overflow traps, with code 1); a plain unsigned
    /// `pow<overflow=trap>` traps on overflow; a saturating signed shift by a
    /// negative amount errors with 3.
    #[test]
    fn test_interpreter_runs_v2_ops_end_to_end() {
        use ir_lang::interp::{self, Outcome, Val};
        let text = r#"module "v2" ptr64
sig s0 = (i32, i32) -> (i32, u32) fast
sig s1 = (u32, u32) -> (u32) fast

func @pow_check s0 export {
b0(v0: i32, v1: i32):
    check pow<overflow=wrap> v0, v1 to b1(res0) error b2(err)
b1(v2: i32):
    v3: u32 = const 0
    ret v2, v3
b2(v4: u32):
    v5: i32 = const 0
    ret v5, v4
}

func @pow_trap s0 export {
b0(v0: i32, v1: i32):
    check pow<overflow=trap> v0, v1 to b1(res0) error b2(err)
b1(v2: i32):
    v3: u32 = const 0
    ret v2, v3
b2(v4: u32):
    v5: i32 = const 0
    ret v5, v4
}

func @shift s0 export {
b0(v0: i32, v1: i32):
    check shr<shift=saturate> v0, v1 to b1(res0) error b2(err)
b1(v2: i32):
    v3: u32 = const 0
    ret v2, v3
b2(v4: u32):
    v5: i32 = const 0
    ret v5, v4
}

func @upow s1 export {
b0(v0: u32, v1: u32):
    v2: u32 = pow<overflow=trap> v0, v1
    ret v2
}
"#;
        let m = ir_lang::parse(text).unwrap();
        m.validate().unwrap();
        assert_eq!(m.to_string(), text);
        let [pc, pt, sh, up] = [0, 1, 2, 3].map(ir_lang::FuncId::from_u32);
        let run = |f, a: i32, b: i32| interp::run(&m, f, &[Val::i32(a), Val::i32(b)]).unwrap();
        assert_eq!(
            run(pc, 3, 4),
            Outcome::Return(vec![Val::i32(81), Val::u32(0)])
        );
        assert_eq!(
            run(pc, 3, -1),
            Outcome::Return(vec![Val::i32(0), Val::u32(6)])
        );
        assert_eq!(
            run(pc, 2, 40),
            Outcome::Return(vec![Val::i32(0), Val::u32(0)]) // wraps to 0
        );
        // Under `trap` a negative exponent still takes the error edge (code 6);
        // only an unrepresentable power traps (code 1).
        assert_eq!(
            run(pt, 2, -1),
            Outcome::Return(vec![Val::i32(0), Val::u32(6)])
        );
        assert_eq!(
            run(pt, 2, 30),
            Outcome::Return(vec![Val::i32(1 << 30), Val::u32(0)])
        );
        assert_eq!(run(pt, 2, 31), Outcome::Trap(1));
        let urun = |a: u32, b: u32| interp::run(&m, up, &[Val::u32(a), Val::u32(b)]).unwrap();
        assert_eq!(urun(3, 4), Outcome::Return(vec![Val::u32(81)]));
        // An exponent with the top bit set is large, not negative.
        assert_eq!(urun(1, u32::MAX), Outcome::Return(vec![Val::u32(1)]));
        assert_eq!(urun(2, u32::MAX), Outcome::Trap(1));
        assert_eq!(
            run(sh, -7, 99),
            Outcome::Return(vec![Val::i32(-1), Val::u32(0)])
        );
        assert_eq!(
            run(sh, -7, -1),
            Outcome::Return(vec![Val::i32(0), Val::u32(3)])
        );
    }
}

fn single(params: &[Type]) -> (Module, ir_lang::FuncId) {
    let mut m = Module::new("rules");
    let f = m
        .declare_function("f", &Signature::new(params, &[]), Linkage::Export)
        .unwrap();
    (m, f)
}

/// Placement: what can raise an error must be in a `check`, and a `check` must
/// hold something that can.
#[test]
fn test_v2_placement_rules() {
    for (ty, policy, op, fallible) in [
        (
            Type::I32,
            Policy::overflow(Overflow::Wrap),
            BinaryOp::Pow,
            true,
        ),
        (
            Type::I32,
            Policy::overflow(Overflow::Error),
            BinaryOp::Pow,
            true,
        ),
        (
            Type::I32,
            Policy::overflow(Overflow::Trap),
            BinaryOp::Pow,
            true,
        ),
        (
            Type::U32,
            Policy::overflow(Overflow::Trap),
            BinaryOp::Pow,
            false,
        ),
        (
            Type::U32,
            Policy::overflow(Overflow::Wrap),
            BinaryOp::Pow,
            false,
        ),
        (
            Type::U32,
            Policy::overflow(Overflow::Error),
            BinaryOp::Pow,
            true,
        ),
        (
            Type::I8,
            Policy::shift(Shift::Saturate),
            BinaryOp::Shl,
            true,
        ),
        (
            Type::I8,
            Policy::shift(Shift::Saturate),
            BinaryOp::Shr,
            true,
        ),
        (
            Type::U8,
            Policy::shift(Shift::Saturate),
            BinaryOp::Shr,
            false,
        ),
        (Type::F64, Policy::NONE, BinaryOp::Pow, false),
    ] {
        assert_eq!(op.can_error(policy, ty), fallible, "{op:?} {ty} {policy}");
        let (mut m, f) = single(&[ty, ty]);
        let mut b = m.build(f).unwrap();
        let (x, y) = (b.param(0).unwrap(), b.param(1).unwrap());
        let plain = b.binary(op, policy, x, y);
        if fallible {
            assert_eq!(
                plain,
                Err(BuildError::Invalid(
                    ValidationErrorKind::ErrorOutsideCheck { op: op.name() }
                ))
            );
        } else {
            assert!(plain.is_ok(), "{op:?} {ty} {policy}: {plain:?}");
        }
        let ok = b.create_block(&[ty]).unwrap();
        let err = b.create_block(&[Type::U32]).unwrap();
        let checked = b.check(
            InstData::Binary {
                op,
                policy,
                args: [x, y],
            },
            BlockCall::with_args(ok, &[BlockArg::Result(0)]),
            BlockCall::with_args(err, &[BlockArg::ErrorCode]),
        );
        if fallible {
            checked.unwrap();
        } else {
            assert_eq!(
                checked,
                Err(BuildError::Invalid(ValidationErrorKind::NotFallible {
                    op: op.name()
                }))
            );
        }
    }
}

#[test]
fn test_v2_policy_fields_are_exact() {
    let (mut m, f) = single(&[Type::F64, Type::I64]);
    let mut b = m.build(f).unwrap();
    let (x, i) = (b.param(0).unwrap(), b.param(1).unwrap());
    // Float pow carries no policy; integer pow carries exactly `overflow`.
    assert!(
        b.binary(BinaryOp::Pow, Policy::overflow(Overflow::Wrap), x, x)
            .is_err()
    );
    assert!(b.binary(BinaryOp::Pow, Policy::NONE, i, i).is_err());
    assert!(
        b.binary(
            BinaryOp::Pow,
            Policy::overflow(Overflow::Trap).with_div_zero(ir_lang::DivZero::Trap),
            i,
            i
        )
        .is_err()
    );
    assert!(b.binary(BinaryOp::Pow, Policy::NONE, x, x).is_ok());
    let need = Policy::required_binary(BinaryOp::Pow, Type::I64);
    assert!(need.overflow && !need.div_zero && !need.shift);
    assert!(Policy::required_binary(BinaryOp::Pow, Type::F32).is_empty());
    // `pow` is not defined on `bool`.
    let c = b.bconst(true).unwrap();
    assert!(b.binary(BinaryOp::Pow, Policy::NONE, c, c).is_err());
}

#[test]
fn test_v2_names_codes_and_text() {
    assert_eq!(BinaryOp::from_name("pow"), Some(BinaryOp::Pow));
    assert_eq!(Shift::from_name("saturate"), Some(Shift::Saturate));
    assert_eq!(OpError::NegativeExponent.code(), 6);
    assert_eq!(OpError::from_code(6), Some(OpError::NegativeExponent));
    assert_eq!(
        OpError::NegativeExponent.to_string(),
        "E0006 NegativeExponent"
    );
    // `promote` is still not representable.
    let text = "module \"m\" ptr64\nsig s0 = (i64) -> (i64) fast\n\nfunc @f s0 export {\nb0(v0: i64):\n    v1: i64 = pow<overflow=promote> v0, v0\n    ret v1\n}\n";
    assert!(ir_lang::parse(text).is_err());
    // A plain signed `pow<overflow=trap>` parses (the parser reads syntax) and
    // round-trips, but does not validate: a negative exponent is an error, not
    // a trap, so the operation belongs in a `check`.
    let signed = text.replace("promote", "trap");
    let m = ir_lang::parse(&signed).unwrap();
    assert_eq!(m.to_string(), signed);
    assert_eq!(
        m.validate().map_err(|e| e.kind().clone()),
        Err(ValidationErrorKind::ErrorOutsideCheck { op: "pow" })
    );
    // On an unsigned type it is a plain instruction.
    let unsigned = signed.replace("i64", "u64");
    let m = ir_lang::parse(&unsigned).unwrap();
    m.validate().unwrap();
    assert_eq!(m.to_string(), unsigned);
}

/// ISSUES P36, pinned for every `overflow` policy × signedness × width: integer
/// `pow` with a negative exponent is `NegativeExponent` (E0006), never a trap
/// (OPS §3 `pow`, §6), so on a signed type every policy needs a `check`; on an
/// unsigned type no exponent is negative, so only `overflow = error` does.
#[test]
fn test_pow_negative_exponent_rule_for_every_policy_and_signedness() {
    let mut pinned = 0;
    for ty in INT_TYPES {
        for ov in [Overflow::Error, Overflow::Wrap, Overflow::Trap] {
            let p = Policy::overflow(ov);
            let fallible = ty.is_signed_int() || ov == Overflow::Error;
            assert_eq!(BinaryOp::Pow.can_error(p, ty), fallible, "{ty} {p}");
            // The validator applies the same rule, both ways round.
            let (mut m, f) = single(&[ty, ty]);
            let mut b = m.build(f).unwrap();
            let (x, y) = (b.param(0).unwrap(), b.param(1).unwrap());
            let plain = b.binary(BinaryOp::Pow, p, x, y);
            assert_eq!(plain.is_ok(), !fallible, "{ty} {p}: {plain:?}");
            let ok = b.create_block(&[ty]).unwrap();
            let err = b.create_block(&[Type::U32]).unwrap();
            let checked = b.check(
                InstData::Binary {
                    op: BinaryOp::Pow,
                    policy: p,
                    args: [x, y],
                },
                BlockCall::with_args(ok, &[BlockArg::Result(0)]),
                BlockCall::with_args(err, &[BlockArg::ErrorCode]),
            );
            assert_eq!(checked.is_ok(), fallible, "{ty} {p}: {checked:?}");
            #[cfg(feature = "interp")]
            {
                use ir_lang::interp::{OpFault, eval_binary};
                // -1 as an exponent: an error on signed types under every
                // policy; on unsigned types the same bits are the largest
                // exponent, so 2 ** MAX overflows per the policy.
                let all_ones = to_bits(ty, -1);
                let got = eval_binary(BinaryOp::Pow, p, ty, 2, all_ones);
                let want = if ty.is_signed_int() {
                    Err(OpFault::Error(OpError::NegativeExponent))
                } else {
                    match ov {
                        Overflow::Wrap => Ok(0),
                        Overflow::Trap => Err(OpFault::Trap(OpError::ArithOverflow)),
                        _ => Err(OpFault::Error(OpError::ArithOverflow)),
                    }
                };
                assert_eq!(got, want, "{ty} 2 ** {all_ones:#x} {p}");
                // A negative base with a non-negative exponent is never E0006.
                if ty.is_signed_int() {
                    assert_eq!(
                        eval_binary(BinaryOp::Pow, p, ty, to_bits(ty, -2), 3),
                        Ok(to_bits(ty, -8)),
                        "{ty} {p}"
                    );
                }
            }
            pinned += 1;
        }
    }
    assert_eq!(pinned, 24);
}
