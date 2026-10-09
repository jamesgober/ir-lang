//! Validator checks that only corrupted internal state can trigger (the builder
//! and the parser never produce it), and a corruption sweep showing the validator
//! is total: it rejects or accepts, and never panics.

use alloc::string::ToString;
use alloc::vec::Vec;

use crate::entity::{Block, FuncId, Inst, NONE, SigId, Value};
use crate::error::ValidationErrorKind as K;
use crate::function::ValueDef;
use crate::inst::Terminator;
use crate::module::{Linkage, Module};
use crate::ops::Overflow;
use crate::types::{Aggregate, Signature, Type};

/// `fn f(x: i32) -> i32 { let y = x + x; y + y }` across two blocks.
fn sample() -> (Module, FuncId) {
    let mut m = Module::new("t");
    let f = m
        .declare_function(
            "f",
            &Signature::new(&[Type::I32], &[Type::I32]),
            Linkage::Export,
        )
        .unwrap_or(FuncId::from_u32(0));
    if let Ok(mut b) = m.build(f) {
        let x = b.param(0).unwrap_or(Value::from_u32(0));
        let y = b.add(x, x, Overflow::Wrap).unwrap_or(x);
        let next = b.create_block(&[Type::I32]).unwrap_or(Block::from_u32(0));
        let _ = b.jump(next, &[y]);
        let _ = b.switch_to(next);
        let p = b.block_param(next, 0).unwrap_or(x);
        let z = b.add(p, p, Overflow::Wrap).unwrap_or(x);
        let _ = b.ret(&[z]);
    }
    (m, f)
}

fn kind_of(m: &Module) -> Option<K> {
    m.validate().err().map(|e| e.kind().clone())
}

fn body(m: &mut Module, f: FuncId) -> &mut crate::function::Function {
    match m.bodies.get_mut(f.index()).and_then(Option::as_mut) {
        Some(b) => b,
        None => unreachable_body(),
    }
}

#[allow(clippy::panic)]
fn unreachable_body() -> ! {
    panic!("the sample always has a body")
}

#[test]
fn test_sample_is_valid() {
    let (m, _) = sample();
    assert_eq!(kind_of(&m), None);
}

#[test]
fn test_duplicate_and_empty_names() {
    let (mut m, _) = sample();
    if let Some(d) = m.decls.funcs.get_mut(0) {
        d.name = "".to_string();
    }
    assert_eq!(kind_of(&m), Some(K::EmptyName));
    let (mut m, _) = sample();
    let _ = m.declare_function("g", &Signature::new(&[], &[]), Linkage::Import);
    if let Some(d) = m.decls.funcs.get_mut(1) {
        d.name = "f".to_string();
    }
    assert!(matches!(kind_of(&m), Some(K::DuplicateName { .. })));
}

#[test]
fn test_unknown_signature_and_bad_aggregate() {
    let (mut m, f) = sample();
    if let Some(d) = m.decls.funcs.get_mut(f.index()) {
        d.sig = SigId::from_u32(40);
    }
    assert!(matches!(kind_of(&m), Some(K::UnknownSignature { .. })));

    let (mut m, _) = sample();
    let _ = m.declare_aggregate(Aggregate::structure(&[Type::I64.into()]));
    if let Some(a) = m.decls.aggs.get_mut(0) {
        a.layout.size = 3;
    }
    assert!(matches!(kind_of(&m), Some(K::BadAggregate { .. })));
}

#[test]
fn test_bad_global() {
    let (mut m, _) = sample();
    let _ = m.declare_global("g", crate::module::Global::zeroed(8, 8));
    if let Some(g) = m.decls.globals.get_mut(0) {
        g.global.init = crate::module::Init::Bytes(alloc::vec![1]);
    }
    assert!(matches!(kind_of(&m), Some(K::BadGlobal { .. })));
}

#[test]
fn test_corrupt_layout_is_detected() {
    let (mut m, f) = sample();
    // Point the first instruction's `next` at itself.
    let b = body(&mut m, f);
    if let Some(n) = b.insts.get_mut(0) {
        n.next = 0;
    }
    assert_eq!(kind_of(&m), Some(K::CorruptLayout));

    let (mut m, f) = sample();
    let b = body(&mut m, f);
    // An instruction claiming a block whose list misses it.
    if let Some(n) = b.insts.get_mut(1) {
        n.block = 0;
    }
    assert_eq!(kind_of(&m), Some(K::CorruptLayout));
}

#[test]
fn test_definition_corruption_is_detected() {
    let (mut m, f) = sample();
    let b = body(&mut m, f);
    if let Some(v) = b.values.get_mut(1) {
        v.def = ValueDef::Param {
            block: Block::from_u32(0),
            index: 0,
        };
    }
    assert!(matches!(
        kind_of(&m),
        Some(K::InconsistentDefinition { .. })
    ));

    let (mut m, f) = sample();
    let b = body(&mut m, f);
    // The second block lists the entry's parameter as its own.
    if let Some(blk) = b.blocks.get_mut(1) {
        blk.params = alloc::vec![Value::from_u32(0)];
    }
    assert!(matches!(
        kind_of(&m),
        Some(K::InconsistentDefinition { .. } | K::DefinedTwice { .. })
    ));

    let (mut m, f) = sample();
    let b = body(&mut m, f);
    if let Some(blk) = b.blocks.get_mut(1) {
        blk.params = alloc::vec![Value::from_u32(900)];
    }
    assert!(matches!(kind_of(&m), Some(K::ValueOutOfRange { .. })));
}

#[test]
fn test_defined_twice_is_detected() {
    let (mut m, f) = sample();
    let b = body(&mut m, f);
    // Two instructions claiming the same result value.
    let r = b.insts.first().map(|n| n.results).unwrap_or_default();
    if let Some(n) = b.insts.get_mut(1) {
        n.results = r;
    }
    if let Some(v) = b.values.get_mut(r.start as usize) {
        v.def = ValueDef::Result {
            inst: Inst::from_u32(0),
            index: 0,
        };
    }
    assert!(matches!(
        kind_of(&m),
        Some(K::DefinedTwice { .. } | K::InconsistentDefinition { .. })
    ));
}

#[test]
fn test_bad_alias_is_detected() {
    let (mut m, f) = sample();
    let b = body(&mut m, f);
    if let Some(v) = b.values.get_mut(1) {
        v.alias = 1;
    }
    assert!(matches!(kind_of(&m), Some(K::BadAlias { .. })));
    let (mut m, f) = sample();
    let b = body(&mut m, f);
    if let Some(v) = b.values.get_mut(1) {
        v.alias = 9_999;
    }
    assert!(matches!(kind_of(&m), Some(K::BadAlias { .. })));
}

#[test]
fn test_entry_out_of_range() {
    let (mut m, f) = sample();
    body(&mut m, f).entry = Block::from_u32(77);
    assert_eq!(kind_of(&m), Some(K::BadEntry));
}

/// Small deterministic generator for the sweep.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) as u32
    }
}

/// Corrupts random fields of a valid function thousands of times; validation
/// must always return (and printing must not panic either).
#[test]
fn test_validator_is_total_under_random_corruption() {
    let mut rng = Lcg(7);
    let mut rejected = 0;
    for _ in 0..4000 {
        let (mut m, f) = sample();
        let b = body(&mut m, f);
        for _ in 0..1 + rng.next() % 3 {
            let pick = rng.next() % 9;
            let small = rng.next() % 8;
            let any = if rng.next() % 4 == 0 { NONE } else { small };
            match pick {
                0 => {
                    let i = (small as usize) % b.insts.len().max(1);
                    if let Some(n) = b.insts.get_mut(i) {
                        n.next = any;
                    }
                }
                1 => {
                    let i = (small as usize) % b.insts.len().max(1);
                    if let Some(n) = b.insts.get_mut(i) {
                        n.block = any;
                    }
                }
                2 => {
                    let i = (small as usize) % b.blocks.len().max(1);
                    if let Some(n) = b.blocks.get_mut(i) {
                        n.first = any;
                    }
                }
                3 => {
                    let i = (small as usize) % b.values.len().max(1);
                    if let Some(v) = b.values.get_mut(i) {
                        v.alias = any;
                    }
                }
                4 => {
                    let i = (small as usize) % b.values.len().max(1);
                    if let Some(v) = b.values.get_mut(i) {
                        v.ty = Type::ALL[(any as usize) % Type::ALL.len()];
                    }
                }
                5 => {
                    let i = (small as usize) % b.blocks.len().max(1);
                    if let Some(n) = b.blocks.get_mut(i) {
                        n.term = match rng.next() % 3 {
                            0 => None,
                            1 => Some(Terminator::Unreachable),
                            _ => n.term.take(),
                        };
                    }
                }
                6 => {
                    let i = (small as usize) % b.insts.len().max(1);
                    if let Some(n) = b.insts.get_mut(i) {
                        n.results.start = any;
                    }
                }
                7 => {
                    let i = (small as usize) % b.blocks.len().max(1);
                    if let Some(n) = b.blocks.get_mut(i) {
                        n.live = !n.live;
                    }
                }
                _ => {
                    let i = (small as usize) % b.insts.len().max(1);
                    if let Some(n) = b.insts.get_mut(i) {
                        n.data
                            .map_operands(|v| Value::from_u32(v.as_u32() ^ (any & 3)));
                    }
                }
            }
        }
        if m.validate().is_err() {
            rejected += 1;
        }
        let _ = m.to_string();
    }
    assert!(rejected > 1000);
    let _: Vec<()> = Vec::new();
}
