//! `ls_pow`: the family's shared float power routine (OPS v2 §4).
//!
//! Specified operation by operation in `_lexersketch/specs/ops-vectors/pow.md`,
//! so every tier computes the same bits: OPS forbids a platform libm for `pow`,
//! whose last bit differs between libraries. The routine uses only IEEE 754
//! binary64 `+ - * /` with round-to-nearest-even, comparisons, and integer
//! arithmetic on bit patterns, evaluated exactly as written (Rust never
//! contracts `a * b + c` into an fma). It needs nothing from `std`, so it is
//! available without the `interp` feature, for constant folding in `no_std`
//! optimizers.
//!
//! The structure is the textbook one, carried in double-double arithmetic:
//! `x^y = e^(y ln x)`, with `ln x` by one Newton step on a double
//! approximation, and `e^z` by reduction to `|r| <= ln2/2`, a scaling by 2^-10,
//! a Taylor series, and ten exact squarings of `e^s - 1`. Integer exponents in
//! `1..=127` take an exact integer path first, which catches every exact
//! midpoint an integer exponent can produce. The spec's 122 vectors are checked
//! bit for bit in `tests/pow_vectors.rs`.

const INV_LN2: f64 = f64::from_bits(0x3FF7_1547_652B_82FE);
/// `ln 2` in three parts: `LN2_HI` has 42 significant bits, so `n * LN2_HI` is
/// exact for every `|n| < 2^11`.
const LN2_HI: f64 = f64::from_bits(0x3FE6_2E42_FEFA_3800);
const LN2_MID: f64 = f64::from_bits(0x3D2E_F357_93C7_6730);
const LN2_LO: f64 = f64::from_bits(0x398F_97B5_7A07_9A19);
/// `1/6` and `1/24` as double-doubles.
const C6: Dd = Dd(
    f64::from_bits(0x3FC5_5555_5555_5555),
    f64::from_bits(0x3C65_5555_5555_5555),
);
const C24: Dd = Dd(
    f64::from_bits(0x3FA5_5555_5555_5555),
    f64::from_bits(0x3C45_5555_5555_5555),
);
/// `1/120`, `1/720`, `1/5040`, `1/40320`, rounded.
const R120: f64 = f64::from_bits(0x3F81_1111_1111_1111);
const R720: f64 = f64::from_bits(0x3F56_C16C_16C1_6C17);
const R5040: f64 = f64::from_bits(0x3F2A_01A0_1A01_A01A);
const R40320: f64 = f64::from_bits(0x3EFA_01A0_1A01_A01A);
/// `sqrt(2)`, rounded: mantissas above it are halved.
const SQRT2: f64 = f64::from_bits(0x3FF6_A09E_667F_3BCD);
/// `2/3, 2/5, ..., 2/23`: the `atanh` series of `ln`, for the first guess.
const LOG_C: [f64; 11] = [
    f64::from_bits(0x3FE5_5555_5555_5555),
    f64::from_bits(0x3FD9_9999_9999_999A),
    f64::from_bits(0x3FD2_4924_9249_2492),
    f64::from_bits(0x3FCC_71C7_1C71_C71C),
    f64::from_bits(0x3FC7_45D1_745D_1746),
    f64::from_bits(0x3FC3_B13B_13B1_3B14),
    f64::from_bits(0x3FC1_1111_1111_1111),
    f64::from_bits(0x3FBE_1E1E_1E1E_1E1E),
    f64::from_bits(0x3FBA_F286_BCA1_AF28),
    f64::from_bits(0x3FB8_6186_1861_8618),
    f64::from_bits(0x3FB6_42C8_590B_2164),
];
/// Veltkamp's splitting constant, `2^27 + 1`.
const SPLIT: f64 = 134_217_729.0;
const TWO_POW_54: f64 = 18_014_398_509_481_984.0;
/// `2^-60`: below it `e^z` rounds to 1.
const TINY_Z: f64 = f64::from_bits(0x3C30_0000_0000_0000);
const MIN_NORMAL: f64 = f64::from_bits(0x0010_0000_0000_0000);
const SIGN: u64 = 1 << 63;
const FRAC: u64 = (1 << 52) - 1;
/// The canonical quiet NaN `ls_pow` returns.
const NAN: f64 = f64::from_bits(0x7FF8_0000_0000_0000);

/// A double-double: the unevaluated sum `hi + lo`, `|lo| <= ulp(hi) / 2`.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Dd(f64, f64);

/// IEEE roundToIntegralTowardZero, from the bits (`core` has no `trunc` on the
/// MSRV).
fn trunc(x: f64) -> f64 {
    let bits = x.to_bits();
    let exp = ((bits >> 52) & 0x7FF) as i32 - 1023;
    if exp >= 52 {
        return x; // integral, infinite, or NaN
    }
    if exp < 0 {
        return f64::from_bits(bits & SIGN); // |x| < 1
    }
    f64::from_bits(bits & !(FRAC >> exp))
}

fn abs(x: f64) -> f64 {
    f64::from_bits(x.to_bits() & !SIGN)
}

/// IEEE roundToIntegralTiesToEven.
fn round_even(x: f64) -> f64 {
    let t = trunc(x);
    let d = abs(x - t);
    // |t| < 2^52 when d != 0, so the conversion is exact.
    let odd = (t as i64) & 1 == 1;
    if d > 0.5 || (d == 0.5 && odd) {
        if x < 0.0 { t - 1.0 } else { t + 1.0 }
    } else {
        t
    }
}

/// Knuth's error-free sum.
#[inline]
fn two_sum(a: f64, b: f64) -> Dd {
    let s = a + b;
    let bb = s - a;
    Dd(s, (a - (s - bb)) + (b - bb))
}

/// Dekker's error-free sum, for `|a| >= |b|` (or `a = 0`).
#[inline]
fn fast_two_sum(a: f64, b: f64) -> Dd {
    let s = a + b;
    Dd(s, b - (s - a))
}

/// Veltkamp's split into two 26-bit halves.
#[inline]
fn split(a: f64) -> (f64, f64) {
    let t = SPLIT * a;
    let hi = t - (t - a);
    (hi, a - hi)
}

/// Dekker's error-free product: `p + e = a * b` exactly (the operands here never
/// overflow the split or underflow the error).
#[inline]
fn two_prod(a: f64, b: f64) -> Dd {
    let p = a * b;
    let (ah, al) = split(a);
    let (bh, bl) = split(b);
    Dd(p, ((ah * bh - p) + ah * bl + al * bh) + al * bl)
}

#[inline]
fn dd_add(a: Dd, b: Dd) -> Dd {
    let s = two_sum(a.0, b.0);
    let t = two_sum(a.1, b.1);
    let s = fast_two_sum(s.0, s.1 + t.0);
    fast_two_sum(s.0, s.1 + t.1)
}

#[inline]
fn dd_mul(a: Dd, b: Dd) -> Dd {
    let p = two_prod(a.0, b.0);
    fast_two_sum(p.0, p.1 + (a.0 * b.1 + a.1 * b.0))
}

/// `e^z` for a double-double `|z| <= 746`: `(n, v)` with `e^z = v * 2^n` and `v`
/// a double-double in about `[0.7, 1.42]`.
fn exp_core(z: Dd) -> (f64, Dd) {
    let n = round_even(z.0 * INV_LN2);
    let a = z.0 - n * LN2_HI;
    let p1 = two_prod(n, LN2_MID);
    let r = dd_add(Dd(a, 0.0), Dd(-p1.0, -p1.1));
    let r = dd_add(r, Dd(z.1 - n * LN2_LO, 0.0));
    let s = Dd(r.0 * 0.000_976_562_5, r.1 * 0.000_976_562_5);
    let q = (((R40320 * s.0 + R5040) * s.0 + R720) * s.0 + R120) * s.0;
    let w = dd_add(C24, Dd(q, 0.0));
    let w = dd_add(dd_mul(w, s), C6);
    let w = dd_add(dd_mul(w, s), Dd(0.5, 0.0));
    let w = dd_add(dd_mul(w, s), Dd(1.0, 0.0));
    let mut u = dd_mul(w, s);
    // e^(2s) - 1 = 2u + u^2: ten squarings undo the scaling by 2^-10 without
    // ever adding 1, so u keeps its relative precision.
    for _ in 0..10 {
        u = dd_add(Dd(2.0 * u.0, 2.0 * u.1), dd_mul(u, u));
    }
    let h = fast_two_sum(1.0, u.0);
    (n, fast_two_sum(h.0, h.1 + u.1))
}

/// `ln x` as a double-double accurate relative to `ln x`, for finite `x > 0`.
fn log_dd(x: f64) -> Dd {
    let (mut x, mut k) = (x, 0i64);
    if x < MIN_NORMAL {
        x *= TWO_POW_54;
        k = -54;
    }
    let bits = x.to_bits();
    let mut e = ((bits >> 52) & 0x7FF) as i64 - 1023;
    let mut m = f64::from_bits((bits & FRAC) | 0x3FF0_0000_0000_0000);
    if m > SQRT2 {
        m *= 0.5;
        e += 1;
    }
    let k = (k + e) as f64;
    // A first guess from the atanh series in plain doubles (relative error about
    // 2^-52), then one Newton step through e^(-y0) in double-double.
    let fm = m - 1.0;
    let t = fm / (2.0 + fm);
    let t2 = t * t;
    let mut p = LOG_C[10];
    for &c in LOG_C[..10].iter().rev() {
        p = p * t2 + c;
    }
    let y0 = t * 2.0 + (t * t2) * p;
    // |y0| is about ln(sqrt 2) at most, so exp_core's n is 0 or -1; the scaling
    // by 2^n is exact either way.
    let (n, ev) = exp_core(Dd(-y0, 0.0));
    let sc = pow2(n as i64);
    let ev = Dd(ev.0 * sc, ev.1 * sc);
    let pr = two_prod(m, ev.0);
    let w = two_sum(pr.0 - 1.0, pr.1 + m * ev.1);
    // ln(1 + w) = w - w^2/2 + O(w^3), and |w| is about 2^-52.
    let corr = w.0 + (w.1 - 0.5 * w.0 * w.0);
    let lm = two_sum(y0, corr);
    let p2 = two_prod(k, LN2_MID);
    let acc = dd_add(Dd(k * LN2_HI, 0.0), p2);
    let acc = dd_add(acc, lm);
    dd_add(acc, Dd(k * LN2_LO, 0.0))
}

/// `2^n` for `-1022 <= n <= 1023`.
#[inline]
fn pow2(n: i64) -> f64 {
    f64::from_bits(((n + 1023) as u64) << 52)
}

/// `v * 2^n` for `v` in about `[0.5, 2]`, with one rounding (the scalings by
/// `2^±1000` are exact).
fn scale(v: f64, n: i64) -> f64 {
    let (mut v, mut n) = (v, n);
    if n > 1000 {
        v *= pow2(1000);
        n -= 1000;
    }
    if n < -1000 {
        v *= pow2(-1000);
        n += 1000;
    }
    v * pow2(n)
}

/// The integer nearest `h + l` (a double-double, `0 <= h <= 2^53`), ties to even.
fn round_dd_int(h: f64, l: f64) -> f64 {
    let k = round_even(h);
    let d = (h - k) + l;
    let odd = round_even(k * 0.5) * 2.0 != k;
    if d > 0.5 || (d == 0.5 && odd) {
        k + 1.0
    } else if d < -0.5 || (d == -0.5 && odd) {
        k - 1.0
    } else {
        k
    }
}

/// `v * 2^n` rounded once, for a double-double `v` from [`exp_core`]: a
/// subnormal result is rounded directly to a multiple of 2^-1074, so it is not
/// rounded twice.
fn finish(v: Dd, n: i64) -> f64 {
    if n > -1022 {
        return scale(v.0 + v.1, n);
    }
    if n + 1074 < -2 {
        return 0.0;
    }
    let sc = pow2(n + 1074);
    let k = round_dd_int(v.0 * sc, v.1 * sc);
    k * f64::from_bits(1)
}

/// Whether `y` is a finite integer.
#[inline]
fn is_int(y: f64) -> bool {
    y.is_finite() && trunc(y) == y
}

/// Whether `y` is an odd integer (every float at or above 2^53 is even).
#[inline]
fn is_odd_int(y: f64) -> bool {
    is_int(y) && abs(y) < 9_007_199_254_740_992.0 && {
        let h = y * 0.5;
        trunc(h) != h
    }
}

/// `x^y` exactly in integers, for finite `x > 0` and an integer `y` in
/// `1..=127` whose power of `x`'s odd significand fits 127 bits, rounded once;
/// `None` otherwise or for a subnormal result.
fn exact_int_pow(x: f64, y: f64) -> Option<f64> {
    let bits = x.to_bits();
    let biased = ((bits >> 52) & 0x7FF) as i64;
    let mut mant = u128::from(bits & FRAC);
    let mut ex = if biased == 0 {
        1
    } else {
        mant |= 1 << 52;
        biased
    } - 1075;
    if mant == 0 {
        return None;
    }
    let tz = mant.trailing_zeros();
    mant >>= tz;
    ex += i64::from(tz);
    let n = y as u32;
    let len = 128 - mant.leading_zeros();
    if len.saturating_mul(n) > 127 {
        return None;
    }
    let mut p: u128 = 1;
    for _ in 0..n {
        // Cannot overflow: the product has at most `len * n <= 127` bits.
        p = p.wrapping_mul(mant);
    }
    let mut e = ex * i64::from(n);
    let length = i64::from(128 - p.leading_zeros());
    if e + length - 1 < -1022 {
        return None;
    }
    if length > 53 {
        let sh = (length - 53) as u32;
        let mut q = p >> sh;
        let rem = p & ((1u128 << sh) - 1);
        let half = 1u128 << (sh - 1);
        if rem > half || (rem == half && q & 1 == 1) {
            q += 1;
        }
        p = q;
        e += i64::from(sh);
    }
    let top = i64::from(127 - p.leading_zeros());
    if e + top > 1023 {
        return Some(f64::INFINITY);
    }
    // p <= 2^53, so it converts exactly; scaled into [1, 2] for `scale`.
    Some(scale(p as f64 * pow2(-top), e + top))
}

/// `ls_pow(x, y)`: the float `pow` of OPS v2, the same bits on every tier.
///
/// The special cases of C99 Annex F come first (`pow(x, ±0) = 1` even for a NaN
/// `x`, `pow(1, y) = 1` even for a NaN `y`, the infinity and signed-zero rules);
/// every other result is computed by the algorithm of
/// `_lexersketch/specs/ops-vectors/pow.md` exactly as written. A NaN result is
/// the canonical quiet NaN `0x7FF8_0000_0000_0000`. The result is the correctly
/// rounded `x^y` except within about 2^-85 (relative) of a rounding boundary,
/// where it is still deterministic.
///
/// `f32` `pow` is `ls_pow` of both operands widened to `f64`, rounded to `f32`
/// once (`ls_pow(a.into(), b.into()) as f32`).
///
/// # Examples
///
/// ```
/// use ir_lang::ls_pow;
///
/// assert_eq!(ls_pow(2.0, 10.0), 1024.0);
/// assert_eq!(ls_pow(4.0, 0.5), 2.0);
/// assert_eq!(ls_pow(f64::NAN, 0.0), 1.0);
/// assert_eq!(ls_pow(-8.0, 1.0 / 3.0).to_bits(), 0x7FF8_0000_0000_0000);
/// // 5^23 is an exact midpoint between two doubles: ties to even.
/// assert_eq!(ls_pow(5.0, 23.0), 11_920_928_955_078_124.0);
/// ```
#[must_use]
pub fn ls_pow(x: f64, y: f64) -> f64 {
    if y == 0.0 || x == 1.0 {
        return 1.0;
    }
    if x.is_nan() || y.is_nan() {
        return NAN;
    }
    let ax = abs(x);
    if y.is_infinite() {
        return if ax == 1.0 {
            1.0
        } else if (ax < 1.0) == (y > 0.0) {
            0.0
        } else {
            f64::INFINITY
        };
    }
    if x.is_infinite() {
        let odd = x < 0.0 && is_odd_int(y);
        return match (y < 0.0, odd) {
            (true, true) => -0.0,
            (true, false) => 0.0,
            (false, true) => f64::NEG_INFINITY,
            (false, false) => f64::INFINITY,
        };
    }
    if x == 0.0 {
        let neg = x.to_bits() & SIGN != 0;
        if is_odd_int(y) {
            return match (y < 0.0, neg) {
                (true, true) => f64::NEG_INFINITY,
                (true, false) => f64::INFINITY,
                (false, _) => x,
            };
        }
        return if y < 0.0 { f64::INFINITY } else { 0.0 };
    }
    let mut sign = 1.0;
    if x < 0.0 {
        if !is_int(y) {
            return NAN;
        }
        if is_odd_int(y) {
            sign = -1.0;
        }
    }
    if is_int(y) && (1.0..=127.0).contains(&y) {
        if let Some(r) = exact_int_pow(ax, y) {
            return sign * r;
        }
    }
    let lg = log_dd(ax);
    let zh = y * lg.0;
    if zh > 710.0 {
        return sign * f64::INFINITY;
    }
    if zh < -746.0 {
        return sign * 0.0;
    }
    if abs(zh) < TINY_Z {
        return sign;
    }
    let p = two_prod(y, lg.0);
    let z = fast_two_sum(p.0, p.1 + y * lg.1);
    let (n, v) = exp_core(z);
    sign * finish(v, n as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_trunc_and_round_even_follow_ieee() {
        assert_eq!(trunc(-2.5).to_bits(), (-2.0f64).to_bits());
        assert_eq!(trunc(-0.5).to_bits(), (-0.0f64).to_bits());
        assert_eq!(trunc(1e300), 1e300);
        assert_eq!(round_even(2.5), 2.0);
        assert_eq!(round_even(3.5), 4.0);
        assert_eq!(round_even(-2.5), -2.0);
        assert_eq!(round_even(-3.5), -4.0);
        assert_eq!(round_even(0.49999999999999994), 0.0);
    }

    #[test]
    fn test_exact_cases_are_exact() {
        assert_eq!(ls_pow(2.0, 10.0), 1024.0);
        assert_eq!(ls_pow(10.0, 22.0), 1e22);
        assert_eq!(ls_pow(2.0, -1.0), 0.5);
        assert_eq!(ls_pow(2.0, -1074.0), f64::from_bits(1));
        assert_eq!(ls_pow(2.0, 1024.0), f64::INFINITY);
        assert_eq!(ls_pow(-2.0, 3.0), -8.0);
        assert_eq!(ls_pow(481.0, 6.0), 12_384_271_322_498_880.0);
    }

    #[test]
    fn test_annex_f_signs() {
        assert_eq!(ls_pow(-0.0, 3.0).to_bits(), (-0.0f64).to_bits());
        assert_eq!(ls_pow(-0.0, -3.0), f64::NEG_INFINITY);
        assert_eq!(
            ls_pow(f64::NEG_INFINITY, -3.0).to_bits(),
            (-0.0f64).to_bits()
        );
        assert_eq!(ls_pow(-1.0, f64::INFINITY), 1.0);
        assert_eq!(ls_pow(-1.0, 1e300), 1.0);
    }
}
