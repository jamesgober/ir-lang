//! Well-formedness validation for a [`Function`], and the errors it reports.

use alloc::vec::Vec;
use core::fmt;

use crate::entity::{Block, Value};
use crate::function::Function;
use crate::inst::{BinOp, Inst, Terminator, UnOp};
use crate::ty::Type;

/// A reason a [`Function`] is not well-formed.
///
/// [`Function::validate`] returns the first one it finds. Every variant names the
/// offending entity so a caller can point a diagnostic at it. The set is
/// `#[non_exhaustive]`: future checks may add variants without it being a breaking
/// change, so a `match` on it must include a wildcard arm.
#[derive(Clone, PartialEq, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum ValidationError {
    /// A block has no terminator. Every block must end in exactly one. Set the
    /// block's terminator before finishing the function.
    MissingTerminator {
        /// The unterminated block.
        block: Block,
    },
    /// A terminator names a block that does not exist in the function. The branch
    /// target is a stale or fabricated handle; rebuild it from a block the function
    /// actually contains.
    BlockOutOfRange {
        /// The out-of-range target.
        block: Block,
    },
    /// An instruction or terminator references a value that does not exist in the
    /// function. The handle is stale or from another function; values are scoped to
    /// the function that minted them.
    ValueOutOfRange {
        /// The out-of-range value.
        value: Value,
    },
    /// A terminator branches to the entry block. Execution enters at the entry, so
    /// it must have no predecessors; route the edge to a fresh block instead.
    EntryBranchTarget {
        /// The block whose terminator targets the entry.
        block: Block,
    },
    /// A jump or branch passes the wrong number of arguments for the target block's
    /// parameters. Pass exactly one argument per target parameter.
    ArgCountMismatch {
        /// The target block whose parameter count was not matched.
        block: Block,
        /// The number of parameters the target declares.
        expected: usize,
        /// The number of arguments the terminator supplied.
        found: usize,
    },
    /// A value was used where a different type was required — mismatched binary
    /// operands, a non-boolean branch condition, a return value of the wrong type,
    /// or a block argument that does not match the target parameter. Fix the
    /// lowering so the value's type matches the position it is used in.
    TypeMismatch {
        /// The value whose type is wrong.
        value: Value,
        /// The type the position required.
        expected: Type,
        /// The type the value actually has.
        found: Type,
    },
    /// An arithmetic operation was applied to a non-numeric value. Add, subtract,
    /// multiply, divide, negate, and the ordering comparisons require
    /// [`Int`](Type::Int) or [`Float`](Type::Float) operands.
    NotNumeric {
        /// The non-numeric value.
        value: Value,
        /// Its actual type.
        found: Type,
    },
    /// A `return` with no value was used in a function whose return type is not
    /// [`Unit`](Type::Unit). Return a value of the declared type.
    ReturnValueExpected {
        /// The return type the function declares.
        expected: Type,
    },
    /// A value was used before its definition reaches the use — its defining block
    /// does not dominate the use, or it is used earlier in the block than it is
    /// defined. In SSA every use must be dominated by its single definition.
    UseBeforeDef {
        /// The value used too early.
        value: Value,
        /// The block the premature use is in.
        block: Block,
    },
    /// A value's recorded definition is inconsistent with the block that lists it:
    /// the value is listed in a block other than the one it records as its
    /// definition site, or it is listed as a parameter while it records an
    /// instruction result (or the reverse). The [`Builder`](crate::Builder) never
    /// produces this; it can only arise from a hand-assembled or corrupt
    /// deserialized function.
    InconsistentDefinition {
        /// The value whose definition does not match where it is listed.
        value: Value,
    },
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ValidationError::MissingTerminator { block } => {
                write!(f, "block {block} has no terminator")
            }
            ValidationError::BlockOutOfRange { block } => {
                write!(f, "terminator targets nonexistent block {block}")
            }
            ValidationError::ValueOutOfRange { value } => {
                write!(f, "reference to nonexistent value {value}")
            }
            ValidationError::EntryBranchTarget { block } => {
                write!(f, "block {block} branches to the entry block")
            }
            ValidationError::ArgCountMismatch {
                block,
                expected,
                found,
            } => write!(
                f,
                "branch to block {block} passes {found} argument(s) but it has {expected} parameter(s)"
            ),
            ValidationError::TypeMismatch {
                value,
                expected,
                found,
            } => write!(
                f,
                "value {value} has type {found} but {expected} was required"
            ),
            ValidationError::NotNumeric { value, found } => {
                write!(
                    f,
                    "value {value} has type {found} but a numeric type was required"
                )
            }
            ValidationError::ReturnValueExpected { expected } => {
                write!(f, "return with no value in a function returning {expected}")
            }
            ValidationError::UseBeforeDef { value, block } => {
                write!(
                    f,
                    "value {value} is used in block {block} before it is defined"
                )
            }
            ValidationError::InconsistentDefinition { value } => {
                write!(
                    f,
                    "value {value} is listed in a block that disagrees with its recorded definition"
                )
            }
        }
    }
}

impl core::error::Error for ValidationError {}

/// Checks that `func` is well-formed, returning the first violation found.
///
/// This is the implementation behind [`Function::validate`]; see that method for the
/// guarantees a passing function provides.
pub(crate) fn validate(func: &Function) -> Result<(), ValidationError> {
    let block_count = func.block_count();

    // The entry must be a real block. A `Builder` always makes one, but a function
    // assembled by other means (a `serde` round-trip of corrupt data) might not, and
    // the dominator computation indexes by the entry, so this is checked first.
    if func.entry().index() >= block_count {
        return Err(ValidationError::BlockOutOfRange {
            block: func.entry(),
        });
    }

    // Structural and type checks over every block, reachable or not.
    let mut succs: Vec<Vec<Block>> = Vec::with_capacity(block_count);
    for block in func.blocks() {
        let term = func
            .terminator(block)
            .ok_or(ValidationError::MissingTerminator { block })?;
        // The block's own definitions are well-formed and consistent with the value
        // table, then its instructions and terminator type-check. Instructions are
        // checked before the terminator so the most upstream error is reported.
        check_block_definitions(func, block)?;
        check_block_insts(func, block)?;
        check_terminator(func, block, term, block_count)?;
        succs.push(successors(term));
    }

    // SSA dominance: every use must be dominated by its definition. Only reachable
    // code can execute, so dominance is checked there; out-of-range and type errors
    // above already covered every block.
    check_dominance(func, &succs)
}

/// Validates a block's terminator: targets exist, the entry is never targeted, the
/// argument counts and types match the target parameters, and the condition or
/// return value has the right type.
fn check_terminator(
    func: &Function,
    block: Block,
    term: &Terminator,
    block_count: usize,
) -> Result<(), ValidationError> {
    match term {
        Terminator::Return(None) => {
            if func.ret() != Type::Unit {
                return Err(ValidationError::ReturnValueExpected {
                    expected: func.ret(),
                });
            }
        }
        Terminator::Return(Some(value)) => {
            let found = value_type(func, *value)?;
            if found != func.ret() {
                return Err(ValidationError::TypeMismatch {
                    value: *value,
                    expected: func.ret(),
                    found,
                });
            }
        }
        Terminator::Jump(target, args) => {
            check_edge(func, *target, args, block, block_count)?;
        }
        Terminator::Branch {
            cond,
            then_block,
            then_args,
            else_block,
            else_args,
        } => {
            let cond_ty = value_type(func, *cond)?;
            if cond_ty != Type::Bool {
                return Err(ValidationError::TypeMismatch {
                    value: *cond,
                    expected: Type::Bool,
                    found: cond_ty,
                });
            }
            check_edge(func, *then_block, then_args, block, block_count)?;
            check_edge(func, *else_block, else_args, block, block_count)?;
        }
    }
    Ok(())
}

/// Validates a single control-flow edge: the target exists, is not the entry, and is
/// passed one argument of the right type per parameter.
fn check_edge(
    func: &Function,
    target: Block,
    args: &[Value],
    from: Block,
    block_count: usize,
) -> Result<(), ValidationError> {
    if target.index() >= block_count {
        return Err(ValidationError::BlockOutOfRange { block: target });
    }
    if target == func.entry() {
        return Err(ValidationError::EntryBranchTarget { block: from });
    }
    let params = func.block_params(target);
    if args.len() != params.len() {
        return Err(ValidationError::ArgCountMismatch {
            block: target,
            expected: params.len(),
            found: args.len(),
        });
    }
    for (&arg, &param) in args.iter().zip(params.iter()) {
        let arg_ty = value_type(func, arg)?;
        let param_ty = value_type(func, param)?;
        if arg_ty != param_ty {
            return Err(ValidationError::TypeMismatch {
                value: arg,
                expected: param_ty,
                found: arg_ty,
            });
        }
    }
    Ok(())
}

/// Checks that every value a block lists — its parameters and its instruction
/// results — is in range and records the same definition site and kind that the
/// listing implies. This is what makes the value table trustworthy for IR that did
/// not come from the [`Builder`](crate::Builder), such as a deserialized function.
fn check_block_definitions(func: &Function, block: Block) -> Result<(), ValidationError> {
    for &value in func.block_params(block) {
        check_definition(func, value, block, DefKind::Param)?;
    }
    for &value in func.insts(block) {
        check_definition(func, value, block, DefKind::Inst)?;
    }
    Ok(())
}

/// Whether a value is defined as a block parameter or as an instruction result.
#[derive(PartialEq)]
enum DefKind {
    Param,
    Inst,
}

/// Verifies one listed value: the handle is in range, its recorded defining block is
/// the block that lists it, and its kind (parameter vs. instruction result) matches
/// the list it appears in.
fn check_definition(
    func: &Function,
    value: Value,
    block: Block,
    kind: DefKind,
) -> Result<(), ValidationError> {
    let def_block = func
        .value_block(value)
        .ok_or(ValidationError::ValueOutOfRange { value })?;
    let def_kind = if func.inst(value).is_some() {
        DefKind::Inst
    } else {
        DefKind::Param
    };
    if def_block != block || def_kind != kind {
        return Err(ValidationError::InconsistentDefinition { value });
    }
    Ok(())
}

/// Type-checks every instruction in a block and confirms each result's recorded type
/// is the type the instruction actually produces.
fn check_block_insts(func: &Function, block: Block) -> Result<(), ValidationError> {
    for &value in func.insts(block) {
        if let Some(inst) = func.inst(value) {
            check_inst(func, inst)?;
            check_result_type(func, value, inst)?;
        }
    }
    Ok(())
}

/// Confirms a value's recorded type is the type its defining instruction produces —
/// the missing half of type safety for IR that did not come from the builder, which
/// always records the right type.
fn check_result_type(func: &Function, value: Value, inst: &Inst) -> Result<(), ValidationError> {
    let expected = match inst {
        Inst::Iconst(_) => Type::Int,
        Inst::Fconst(_) => Type::Float,
        Inst::Bconst(_) => Type::Bool,
        Inst::Bin(op, lhs, _) => {
            if op.is_comparison() || op.is_logical() {
                Type::Bool
            } else {
                value_type(func, *lhs)?
            }
        }
        Inst::Un(UnOp::Neg, operand) => value_type(func, *operand)?,
        Inst::Un(UnOp::Not, _) => Type::Bool,
    };
    let found = value_type(func, value)?;
    if found != expected {
        return Err(ValidationError::TypeMismatch {
            value,
            expected,
            found,
        });
    }
    Ok(())
}

/// Type-checks one instruction's operands.
fn check_inst(func: &Function, inst: &Inst) -> Result<(), ValidationError> {
    match inst {
        Inst::Iconst(_) | Inst::Fconst(_) | Inst::Bconst(_) => Ok(()),
        Inst::Bin(op, lhs, rhs) => check_bin(func, *op, *lhs, *rhs),
        Inst::Un(op, operand) => check_un(func, *op, *operand),
    }
}

/// Type-checks a binary operation: operands match each other, and satisfy the
/// numeric or boolean requirement the operation imposes.
fn check_bin(func: &Function, op: BinOp, lhs: Value, rhs: Value) -> Result<(), ValidationError> {
    let lhs_ty = value_type(func, lhs)?;
    let rhs_ty = value_type(func, rhs)?;
    if lhs_ty != rhs_ty {
        return Err(ValidationError::TypeMismatch {
            value: rhs,
            expected: lhs_ty,
            found: rhs_ty,
        });
    }
    if op.is_logical() {
        if lhs_ty != Type::Bool {
            return Err(ValidationError::TypeMismatch {
                value: lhs,
                expected: Type::Bool,
                found: lhs_ty,
            });
        }
    } else if requires_numeric(op) && !lhs_ty.is_numeric() {
        return Err(ValidationError::NotNumeric {
            value: lhs,
            found: lhs_ty,
        });
    }
    Ok(())
}

/// Whether a binary operation requires numeric operands. The arithmetic operations
/// and the ordering comparisons do; equality (`eq`, `ne`) accepts any matching type.
fn requires_numeric(op: BinOp) -> bool {
    matches!(
        op,
        BinOp::Add
            | BinOp::Sub
            | BinOp::Mul
            | BinOp::Div
            | BinOp::Lt
            | BinOp::Le
            | BinOp::Gt
            | BinOp::Ge
    )
}

/// Type-checks a unary operation.
fn check_un(func: &Function, op: UnOp, operand: Value) -> Result<(), ValidationError> {
    let ty = value_type(func, operand)?;
    match op {
        UnOp::Neg if !ty.is_numeric() => Err(ValidationError::NotNumeric {
            value: operand,
            found: ty,
        }),
        UnOp::Not if ty != Type::Bool => Err(ValidationError::TypeMismatch {
            value: operand,
            expected: Type::Bool,
            found: ty,
        }),
        _ => Ok(()),
    }
}

/// Reads a value's type, mapping an out-of-range handle to
/// [`ValidationError::ValueOutOfRange`].
fn value_type(func: &Function, value: Value) -> Result<Type, ValidationError> {
    func.value_type(value)
        .ok_or(ValidationError::ValueOutOfRange { value })
}

/// Collects the control-flow successors of a block from its terminator.
fn successors(term: &Terminator) -> Vec<Block> {
    let mut out = Vec::new();
    term.each_successor(|b| out.push(b));
    out
}

/// A node-entry or node-exit step in the iterative dominator-tree walk.
enum Visit {
    Enter(usize),
    Exit(usize),
}

/// Checks the SSA dominance property over the reachable control-flow graph: every
/// value a reachable block uses is reached by its single definition.
///
/// A single pre-order walk of the dominator tree carries one availability set: a
/// block's definitions are added to it on entry and removed on exit, so a value is
/// visible in exactly the block that defines it and the blocks that block dominates,
/// never in a sibling subtree. The whole check is therefore linear in the size of the
/// function with no per-block allocation, and the walk uses an explicit stack so a
/// deeply nested dominator tree cannot overflow the call stack.
fn check_dominance(func: &Function, succs: &[Vec<Block>]) -> Result<(), ValidationError> {
    let n = succs.len();
    let entry = func.entry().index();
    let idom = compute_idoms(entry, n, succs);

    // The dominator-tree children of each reachable block. `idom` is `Some` exactly
    // for reachable blocks, so unreachable code never enters the walk; it has no
    // definitions that can reach a use that executes.
    let mut children: Vec<Vec<usize>> = alloc::vec![Vec::new(); n];
    for (b, dom) in idom.iter().enumerate() {
        if let Some(parent) = *dom {
            if b != entry {
                children[parent].push(b);
            }
        }
    }

    let mut available = alloc::vec![false; func.value_count()];
    let mut stack = alloc::vec![Visit::Enter(entry)];
    while let Some(visit) = stack.pop() {
        match visit {
            Visit::Exit(b) => {
                let block = Block::from_raw(b as u32);
                for &value in func.block_params(block) {
                    clear_available(&mut available, value);
                }
                for &value in func.insts(block) {
                    clear_available(&mut available, value);
                }
            }
            Visit::Enter(b) => {
                let block = Block::from_raw(b as u32);
                // A block's parameters are available throughout it and its subtree.
                for &param in func.block_params(block) {
                    set_available(&mut available, param);
                }
                // Walk in program order: each operand must already be available, then
                // the result it defines becomes available.
                for &value in func.insts(block) {
                    if let Some(inst) = func.inst(value) {
                        check_operands_available(inst, &available, block)?;
                    }
                    set_available(&mut available, value);
                }
                if let Some(term) = func.terminator(block) {
                    check_terminator_operands_available(term, &available, block)?;
                }
                // Exit runs after every descendant, undoing this block's definitions.
                stack.push(Visit::Exit(b));
                for &child in &children[b] {
                    stack.push(Visit::Enter(child));
                }
            }
        }
    }
    Ok(())
}

fn set_available(available: &mut [bool], value: Value) {
    if let Some(slot) = available.get_mut(value.index()) {
        *slot = true;
    }
}

fn clear_available(available: &mut [bool], value: Value) {
    if let Some(slot) = available.get_mut(value.index()) {
        *slot = false;
    }
}

fn is_available(available: &[bool], value: Value) -> bool {
    available.get(value.index()).copied().unwrap_or(false)
}

/// Verifies each operand of an instruction is available at its use site.
fn check_operands_available(
    inst: &Inst,
    available: &[bool],
    block: Block,
) -> Result<(), ValidationError> {
    match inst {
        Inst::Iconst(_) | Inst::Fconst(_) | Inst::Bconst(_) => Ok(()),
        Inst::Bin(_, lhs, rhs) => {
            require_available(*lhs, available, block)?;
            require_available(*rhs, available, block)
        }
        Inst::Un(_, operand) => require_available(*operand, available, block),
    }
}

/// Verifies each operand of a terminator is available at its use site.
fn check_terminator_operands_available(
    term: &Terminator,
    available: &[bool],
    block: Block,
) -> Result<(), ValidationError> {
    match term {
        Terminator::Return(None) => Ok(()),
        Terminator::Return(Some(value)) => require_available(*value, available, block),
        Terminator::Jump(_, args) => {
            for &arg in args {
                require_available(arg, available, block)?;
            }
            Ok(())
        }
        Terminator::Branch {
            cond,
            then_args,
            else_args,
            ..
        } => {
            require_available(*cond, available, block)?;
            for &arg in then_args.iter().chain(else_args.iter()) {
                require_available(arg, available, block)?;
            }
            Ok(())
        }
    }
}

fn require_available(
    value: Value,
    available: &[bool],
    block: Block,
) -> Result<(), ValidationError> {
    if is_available(available, value) {
        Ok(())
    } else {
        Err(ValidationError::UseBeforeDef { value, block })
    }
}

/// Computes immediate dominators with the Cooper–Harvey–Kennedy algorithm. The
/// returned vector holds `Some(idom)` for every block reachable from the entry
/// (the entry's immediate dominator is itself) and `None` for unreachable blocks.
fn compute_idoms(entry: usize, n: usize, succs: &[Vec<Block>]) -> Vec<Option<usize>> {
    let postorder = postorder(entry, n, succs);
    let mut po_num = alloc::vec![usize::MAX; n];
    for (i, &b) in postorder.iter().enumerate() {
        po_num[b] = i;
    }

    let mut preds: Vec<Vec<usize>> = alloc::vec![Vec::new(); n];
    for (b, block_succs) in succs.iter().enumerate() {
        for s in block_succs {
            if let Some(slot) = preds.get_mut(s.index()) {
                slot.push(b);
            }
        }
    }

    let mut idom = alloc::vec![None; n];
    idom[entry] = Some(entry);

    // Process in reverse postorder until the immediate dominators stop changing.
    let rpo: Vec<usize> = postorder.iter().rev().copied().collect();
    let mut changed = true;
    while changed {
        changed = false;
        for &b in &rpo {
            if b == entry {
                continue;
            }
            let mut new_idom: Option<usize> = None;
            for &p in &preds[b] {
                if idom[p].is_some() {
                    new_idom = Some(match new_idom {
                        None => p,
                        Some(cur) => intersect(p, cur, &idom, &po_num, entry),
                    });
                }
            }
            if idom[b] != new_idom {
                idom[b] = new_idom;
                changed = true;
            }
        }
    }
    idom
}

/// Walks the two dominator-tree fingers up by postorder number until they meet — the
/// nearest common dominator of `a` and `b`.
fn intersect(
    mut a: usize,
    mut b: usize,
    idom: &[Option<usize>],
    po_num: &[usize],
    entry: usize,
) -> usize {
    while a != b {
        while po_num[a] < po_num[b] {
            a = idom[a].unwrap_or(entry);
        }
        while po_num[b] < po_num[a] {
            b = idom[b].unwrap_or(entry);
        }
    }
    a
}

/// Returns the blocks reachable from the entry in postorder (children before
/// parents), computed with an explicit stack so a deep graph cannot overflow.
fn postorder(entry: usize, n: usize, succs: &[Vec<Block>]) -> Vec<usize> {
    let mut visited = alloc::vec![false; n];
    let mut order = Vec::new();
    let mut stack: Vec<(usize, usize)> = Vec::new();

    if entry >= n {
        return order;
    }
    visited[entry] = true;
    stack.push((entry, 0));

    while let Some(&(b, i)) = stack.last() {
        if i < succs[b].len() {
            let top = stack.len() - 1;
            stack[top].1 += 1;
            let s = succs[b][i].index();
            if s < n && !visited[s] {
                visited[s] = true;
                stack.push((s, 0));
            }
        } else {
            order.push(b);
            stack.pop();
        }
    }
    order
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "tests assert on specific error variants; a wrong variant should fail loudly"
)]
mod tests {
    use crate::function::{BlockData, Function, ValueData, ValueDef};
    use crate::inst::Inst;
    use crate::{BinOp, Block, Builder, Terminator, Type, UnOp, Value};

    use super::ValidationError;
    use alloc::string::ToString;
    use alloc::vec;

    #[test]
    fn test_valid_straight_line_function_passes() {
        let mut b = Builder::new("add", &[Type::Int, Type::Int], Type::Int);
        let x = b.block_params(b.entry())[0];
        let y = b.block_params(b.entry())[1];
        let sum = b.bin(BinOp::Add, x, y);
        b.ret(Some(sum));
        assert_eq!(b.finish().validate(), Ok(()));
    }

    #[test]
    fn test_valid_diamond_with_block_params_passes() {
        let mut b = Builder::new("max", &[Type::Int, Type::Int], Type::Int);
        let a = b.block_params(b.entry())[0];
        let c = b.block_params(b.entry())[1];
        let join = b.create_block(&[Type::Int]);
        let then_blk = b.create_block(&[]);
        let else_blk = b.create_block(&[]);
        let cond = b.bin(BinOp::Lt, a, c);
        b.branch(cond, then_blk, &[], else_blk, &[]);
        b.switch_to(then_blk);
        b.jump(join, &[c]);
        b.switch_to(else_blk);
        b.jump(join, &[a]);
        b.switch_to(join);
        let r = b.block_params(join)[0];
        b.ret(Some(r));
        assert_eq!(b.finish().validate(), Ok(()));
    }

    #[test]
    fn test_valid_loop_passes() {
        // entry -> header(i); header: branch back to itself or exit.
        let mut b = Builder::new("loop", &[Type::Int], Type::Unit);
        let start = b.block_params(b.entry())[0];
        let header = b.create_block(&[Type::Int]);
        let body = b.create_block(&[]);
        let exit = b.create_block(&[]);
        b.jump(header, &[start]);
        b.switch_to(header);
        let i = b.block_params(header)[0];
        let zero = b.iconst(0);
        let more = b.bin(BinOp::Gt, i, zero);
        b.branch(more, body, &[], exit, &[]);
        b.switch_to(body);
        let one = b.iconst(1);
        let next = b.bin(BinOp::Sub, i, one);
        b.jump(header, &[next]);
        b.switch_to(exit);
        b.ret(None);
        assert_eq!(b.finish().validate(), Ok(()));
    }

    #[test]
    fn test_missing_terminator_is_rejected() {
        let b = Builder::new("f", &[], Type::Unit);
        // entry never gets a terminator.
        let func = b.finish();
        assert_eq!(
            func.validate(),
            Err(ValidationError::MissingTerminator {
                block: func.entry()
            })
        );
    }

    #[test]
    fn test_arg_count_mismatch_is_rejected() {
        let mut b = Builder::new("f", &[], Type::Int);
        let exit = b.create_block(&[Type::Int]);
        let n = b.iconst(1);
        b.jump(exit, &[]); // exit needs one argument
        b.switch_to(exit);
        let r = b.block_params(exit)[0];
        b.ret(Some(r));
        let _ = n;
        assert_eq!(
            b.finish().validate(),
            Err(ValidationError::ArgCountMismatch {
                block: exit,
                expected: 1,
                found: 0,
            })
        );
    }

    #[test]
    fn test_operand_type_mismatch_is_rejected() {
        let mut b = Builder::new("f", &[Type::Int, Type::Bool], Type::Int);
        let x = b.block_params(b.entry())[0];
        let flag = b.block_params(b.entry())[1];
        let bad = b.bin(BinOp::Add, x, flag); // int + bool
        b.ret(Some(bad));
        assert_eq!(
            b.finish().validate(),
            Err(ValidationError::TypeMismatch {
                value: flag,
                expected: Type::Int,
                found: Type::Bool,
            })
        );
    }

    #[test]
    fn test_non_numeric_arithmetic_is_rejected() {
        let mut b = Builder::new("f", &[Type::Bool, Type::Bool], Type::Bool);
        let p = b.block_params(b.entry())[0];
        let q = b.block_params(b.entry())[1];
        let bad = b.bin(BinOp::Mul, p, q); // bool * bool
        b.ret(Some(bad));
        assert_eq!(
            b.finish().validate(),
            Err(ValidationError::NotNumeric {
                value: p,
                found: Type::Bool,
            })
        );
    }

    #[test]
    fn test_non_bool_condition_is_rejected() {
        let mut b = Builder::new("f", &[Type::Int], Type::Unit);
        let x = b.block_params(b.entry())[0];
        let yes = b.create_block(&[]);
        let no = b.create_block(&[]);
        b.branch(x, yes, &[], no, &[]); // condition is int
        b.switch_to(yes);
        b.ret(None);
        b.switch_to(no);
        b.ret(None);
        assert_eq!(
            b.finish().validate(),
            Err(ValidationError::TypeMismatch {
                value: x,
                expected: Type::Bool,
                found: Type::Int,
            })
        );
    }

    #[test]
    fn test_return_type_mismatch_is_rejected() {
        let mut b = Builder::new("f", &[Type::Bool], Type::Int);
        let flag = b.block_params(b.entry())[0];
        b.ret(Some(flag)); // returns bool, declared int
        assert_eq!(
            b.finish().validate(),
            Err(ValidationError::TypeMismatch {
                value: flag,
                expected: Type::Int,
                found: Type::Bool,
            })
        );
    }

    #[test]
    fn test_return_without_value_in_non_unit_function_is_rejected() {
        let mut b = Builder::new("f", &[], Type::Int);
        b.ret(None);
        assert_eq!(
            b.finish().validate(),
            Err(ValidationError::ReturnValueExpected {
                expected: Type::Int
            })
        );
    }

    #[test]
    fn test_branch_to_entry_is_rejected() {
        let mut b = Builder::new("f", &[], Type::Unit);
        let entry = b.entry();
        b.jump(entry, &[]); // self-loop onto the entry
        assert_eq!(
            b.finish().validate(),
            Err(ValidationError::EntryBranchTarget { block: entry })
        );
    }

    #[test]
    fn test_use_before_def_across_blocks_is_rejected() {
        // A value defined in a block that does not dominate the use.
        let mut b = Builder::new("f", &[], Type::Int);
        let entry = b.entry();
        let other = b.create_block(&[]);
        b.switch_to(other);
        let v = b.iconst(7); // defined in unreachable `other`
        b.ret(Some(v));
        b.switch_to(entry);
        b.ret(Some(v)); // entry uses a value it cannot see
        assert_eq!(
            b.finish().validate(),
            Err(ValidationError::UseBeforeDef {
                value: v,
                block: entry
            })
        );
    }

    #[test]
    fn test_value_defined_in_sibling_branch_is_rejected() {
        // A value defined only in the `then` arm cannot be used in the `else` arm:
        // neither branch dominates the other.
        let mut b = Builder::new("f", &[Type::Bool], Type::Int);
        let cond = b.block_params(b.entry())[0];
        let then_blk = b.create_block(&[]);
        let else_blk = b.create_block(&[]);
        let join = b.create_block(&[Type::Int]);
        b.branch(cond, then_blk, &[], else_blk, &[]);

        b.switch_to(then_blk);
        let secret = b.iconst(7); // defined only here
        b.jump(join, &[secret]);

        b.switch_to(else_blk);
        b.jump(join, &[secret]); // ...but used here, in the sibling branch

        b.switch_to(join);
        let r = b.block_params(join)[0];
        b.ret(Some(r));

        assert_eq!(
            b.finish().validate(),
            Err(ValidationError::UseBeforeDef {
                value: secret,
                block: else_blk,
            })
        );
    }

    #[test]
    fn test_value_from_dominating_block_is_visible_deep_below() {
        // A value defined in the entry is visible through nested control flow, even
        // several dominator-tree levels down.
        let mut b = Builder::new("f", &[Type::Int], Type::Int);
        let base = b.iconst(100); // defined in entry, dominates everything
        let outer_then = b.create_block(&[]);
        let outer_else = b.create_block(&[]);
        let inner_then = b.create_block(&[]);
        let inner_else = b.create_block(&[]);
        let join = b.create_block(&[Type::Int]);

        let p = b.block_params(b.entry())[0];
        let zero = b.iconst(0);
        let c0 = b.bin(BinOp::Gt, p, zero);
        b.branch(c0, outer_then, &[], outer_else, &[]);

        b.switch_to(outer_then);
        let c1 = b.bin(BinOp::Lt, p, base); // uses `base` from entry
        b.branch(c1, inner_then, &[], inner_else, &[]);

        b.switch_to(inner_then);
        b.jump(join, &[base]); // uses `base` two levels down
        b.switch_to(inner_else);
        b.jump(join, &[base]);

        b.switch_to(outer_else);
        b.jump(join, &[base]);

        b.switch_to(join);
        let r = b.block_params(join)[0];
        b.ret(Some(r));

        assert_eq!(b.finish().validate(), Ok(()));
    }

    #[test]
    fn test_out_of_range_value_is_rejected() {
        // White-box: assemble a function that references a nonexistent value.
        let entry = Block::from_raw(0);
        let blocks = vec![BlockData {
            params: vec![],
            insts: vec![],
            term: Some(Terminator::Return(Some(Value::from_raw(5)))),
        }];
        let func = Function::from_parts(
            "f".to_string(),
            vec![],
            Type::Int,
            entry,
            blocks,
            vec![], // no values exist
        );
        assert_eq!(
            func.validate(),
            Err(ValidationError::ValueOutOfRange {
                value: Value::from_raw(5)
            })
        );
    }

    #[test]
    fn test_empty_function_is_rejected_without_panicking() {
        // White-box: a function with no blocks at all. The entry handle points past
        // the (empty) block list, so this must be a defined error, not a panic.
        let func = Function::from_parts(
            "empty".to_string(),
            vec![],
            Type::Unit,
            Block::from_raw(0),
            vec![],
            vec![],
        );
        assert_eq!(
            func.validate(),
            Err(ValidationError::BlockOutOfRange {
                block: Block::from_raw(0)
            })
        );
    }

    #[test]
    fn test_out_of_range_entry_is_rejected_without_panicking() {
        // White-box: a corrupt entry index, as a bad `serde` payload could produce.
        let blocks = vec![BlockData {
            params: vec![],
            insts: vec![],
            term: Some(Terminator::Return(None)),
        }];
        let func = Function::from_parts(
            "f".to_string(),
            vec![],
            Type::Unit,
            Block::from_raw(7), // no such block
            blocks,
            vec![],
        );
        assert_eq!(
            func.validate(),
            Err(ValidationError::BlockOutOfRange {
                block: Block::from_raw(7)
            })
        );
    }

    #[test]
    fn test_out_of_range_block_is_rejected() {
        // White-box: a terminator jumping to a block that does not exist.
        let entry = Block::from_raw(0);
        let blocks = vec![BlockData {
            params: vec![],
            insts: vec![],
            term: Some(Terminator::Jump(Block::from_raw(9), vec![])),
        }];
        let func = Function::from_parts("f".to_string(), vec![], Type::Unit, entry, blocks, vec![]);
        assert_eq!(
            func.validate(),
            Err(ValidationError::BlockOutOfRange {
                block: Block::from_raw(9)
            })
        );
    }

    #[test]
    fn test_wrong_result_type_is_rejected() {
        // White-box: an instruction whose recorded result type is not what it
        // produces — `iconst` claiming to be a `bool`. The builder never does this,
        // but a corrupt serde payload could.
        let entry = Block::from_raw(0);
        let values = vec![ValueData {
            ty: Type::Bool, // wrong: iconst produces Int
            def: ValueDef::Inst(entry, Inst::Iconst(1)),
        }];
        let blocks = vec![BlockData {
            params: vec![],
            insts: vec![Value::from_raw(0)],
            term: Some(Terminator::Return(Some(Value::from_raw(0)))),
        }];
        let func = Function::from_parts("f".to_string(), vec![], Type::Bool, entry, blocks, values);
        assert_eq!(
            func.validate(),
            Err(ValidationError::TypeMismatch {
                value: Value::from_raw(0),
                expected: Type::Int,
                found: Type::Bool,
            })
        );
    }

    #[test]
    fn test_value_listed_as_param_but_defined_as_inst_is_rejected() {
        // White-box: a value listed in a block's parameters while its recorded
        // definition is an instruction result.
        let entry = Block::from_raw(0);
        let values = vec![ValueData {
            ty: Type::Int,
            def: ValueDef::Inst(entry, Inst::Iconst(0)), // says Inst...
        }];
        let blocks = vec![BlockData {
            params: vec![Value::from_raw(0)], // ...but listed as a parameter
            insts: vec![],
            term: Some(Terminator::Return(None)),
        }];
        let func = Function::from_parts("f".to_string(), vec![], Type::Unit, entry, blocks, values);
        assert_eq!(
            func.validate(),
            Err(ValidationError::InconsistentDefinition {
                value: Value::from_raw(0)
            })
        );
    }

    #[test]
    fn test_out_of_range_block_param_handle_is_rejected() {
        // White-box: a block parameter naming a value that does not exist.
        let entry = Block::from_raw(0);
        let blocks = vec![BlockData {
            params: vec![Value::from_raw(9)], // no such value
            insts: vec![],
            term: Some(Terminator::Return(None)),
        }];
        let func = Function::from_parts("f".to_string(), vec![], Type::Unit, entry, blocks, vec![]);
        assert_eq!(
            func.validate(),
            Err(ValidationError::ValueOutOfRange {
                value: Value::from_raw(9)
            })
        );
    }

    #[test]
    fn test_not_operator_requires_bool() {
        let mut b = Builder::new("f", &[Type::Int], Type::Int);
        let x = b.block_params(b.entry())[0];
        let bad = b.un(UnOp::Not, x); // not on int
        b.ret(Some(bad));
        assert!(matches!(
            b.finish().validate(),
            Err(ValidationError::TypeMismatch { .. })
        ));
    }

    #[test]
    fn test_error_display_is_human_readable() {
        let e = ValidationError::MissingTerminator {
            block: Block::from_raw(2),
        };
        assert_eq!(e.to_string(), "block b2 has no terminator");
    }
}
