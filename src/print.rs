//! The deterministic textual form (see `specs/IR.md` §11 for the grammar).

use core::fmt::{self, Write};

use crate::entity::{FuncId, GlobalId, Value, ValueRange};
use crate::function::Function;
use crate::inst::{Attrs, BlockCall, Call, CallTarget, InstData, Terminator};
use crate::module::{Init, Module, SymbolRef};
use crate::types::{Aggregate, PointerWidth, Type};

/// Writes `s` as a double-quoted string with `\\`, `\"`, `\n`, `\t`, `\r`, and
/// `\u{..}` escapes for other control characters.
pub(crate) fn write_quoted(f: &mut impl Write, s: &str) -> fmt::Result {
    f.write_char('"')?;
    for c in s.chars() {
        match c {
            '\\' => f.write_str("\\\\")?,
            '"' => f.write_str("\\\"")?,
            '\n' => f.write_str("\\n")?,
            '\t' => f.write_str("\\t")?,
            '\r' => f.write_str("\\r")?,
            c if c.is_control() => write!(f, "\\u{{{:x}}}", c as u32)?,
            c => f.write_char(c)?,
        }
    }
    f.write_char('"')
}

/// Whether a symbol name can be printed without quotes.
pub(crate) fn is_bare_symbol(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '$')
}

fn write_symbol(f: &mut impl Write, name: &str) -> fmt::Result {
    f.write_char('@')?;
    if is_bare_symbol(name) {
        f.write_str(name)
    } else {
        write_quoted(f, name)
    }
}

fn func_name(m: &Module, func: FuncId, f: &mut impl Write) -> fmt::Result {
    match m.decls.funcs.get(func.index()) {
        Some(d) => write_symbol(f, &d.name),
        None => write!(f, "@\"?{func}\""),
    }
}

fn global_name(m: &Module, global: GlobalId, f: &mut impl Write) -> fmt::Result {
    match m.decls.globals.get(global.index()) {
        Some(g) => write_symbol(f, &g.name),
        None => write!(f, "@\"?{global}\""),
    }
}

/// Writes a constant's bits in the form the parser reads back for `ty`.
pub(crate) fn write_const(
    f: &mut impl Write,
    ty: Type,
    bits: u64,
    ptr: PointerWidth,
) -> fmt::Result {
    match ty {
        Type::Bool if bits <= 1 => f.write_str(if bits == 1 { "true" } else { "false" }),
        Type::Ptr | Type::Ref if bits == 0 => f.write_str("null"),
        Type::F64 => write_f64(f, f64::from_bits(bits), bits),
        Type::F32 if bits >> 32 == 0 => write_f32(f, f32::from_bits(bits as u32), bits),
        t if t.is_int() => {
            let width = t.bits(ptr);
            if width < 64 && bits >> width != 0 {
                return write!(f, "{bits:#x}");
            }
            if t.is_signed_int() {
                let shift = 64 - width;
                write!(f, "{}", ((bits << shift) as i64) >> shift)
            } else {
                write!(f, "{bits}")
            }
        }
        _ => write!(f, "{bits:#x}"),
    }
}

fn write_f64(f: &mut impl Write, x: f64, bits: u64) -> fmt::Result {
    if x.is_nan() {
        write!(f, "nan:{bits:#x}")
    } else if x.is_infinite() {
        f.write_str(if x > 0.0 { "inf" } else { "-inf" })
    } else {
        write!(f, "{x:?}")
    }
}

fn write_f32(f: &mut impl Write, x: f32, bits: u64) -> fmt::Result {
    if x.is_nan() {
        write!(f, "nan:{bits:#x}")
    } else if x.is_infinite() {
        f.write_str(if x > 0.0 { "inf" } else { "-inf" })
    } else {
        write!(f, "{x:?}")
    }
}

fn write_values(f: &mut impl Write, func: &Function, values: &[Value]) -> fmt::Result {
    for (i, &v) in values.iter().enumerate() {
        if i > 0 {
            f.write_str(", ")?;
        }
        write!(f, "{}", func.resolve(v))?;
    }
    Ok(())
}

fn write_dest(f: &mut impl Write, func: &Function, dest: &BlockCall) -> fmt::Result {
    write!(f, "{}", dest.block)?;
    if !dest.args.is_empty() {
        f.write_char('(')?;
        for (i, a) in dest.args.iter().enumerate() {
            if i > 0 {
                f.write_str(", ")?;
            }
            match a {
                crate::inst::BlockArg::Value(v) => write!(f, "{}", func.resolve(*v))?,
                other => write!(f, "{other}")?,
            }
        }
        f.write_char(')')?;
    }
    Ok(())
}

fn write_call(f: &mut impl Write, m: &Module, func: &Function, call: &Call) -> fmt::Result {
    match call.target {
        CallTarget::Direct(callee) => func_name(m, callee, f)?,
        CallTarget::Indirect { sig, callee } => write!(f, "{sig} {}", func.resolve(callee))?,
    }
    f.write_char('(')?;
    write_values(f, func, &call.args)?;
    f.write_char(')')?;
    if !call.gc.is_empty() {
        f.write_str(" gc(")?;
        write_values(f, func, &call.gc)?;
        f.write_char(')')?;
    }
    write_attrs(f, call.attrs)
}

fn write_attrs(f: &mut impl Write, attrs: Attrs) -> fmt::Result {
    if attrs.is_empty() {
        Ok(())
    } else {
        write!(f, " {attrs}")
    }
}

/// Writes an instruction's operation (everything after `results =`). `conv_to`
/// prints a conversion's target type, which a `check` needs (it has no result
/// value to carry the type).
fn write_op(
    f: &mut impl Write,
    m: &Module,
    func: &Function,
    data: &InstData,
    conv_to: bool,
) -> fmt::Result {
    let r = |v: Value| func.resolve(v);
    f.write_str(data.opcode())?;
    match data {
        InstData::Const { ty, bits } => {
            f.write_char(' ')?;
            write_const(f, *ty, *bits, m.decls.ptr)
        }
        InstData::Unary { policy, arg, .. } => write!(f, "{policy} {}", r(*arg)),
        InstData::Binary { policy, args, .. } => {
            write!(f, "{policy} {}, {}", r(args[0]), r(args[1]))
        }
        InstData::Fma { args } => write!(f, " {}, {}, {}", r(args[0]), r(args[1]), r(args[2])),
        InstData::Compare { args, .. } => write!(f, " {}, {}", r(args[0]), r(args[1])),
        InstData::Convert {
            policy, to, arg, ..
        } => {
            write!(f, "{policy} {}", r(*arg))?;
            if conv_to {
                write!(f, " as {to}")?;
            }
            Ok(())
        }
        InstData::Select { cond, args } => {
            write!(f, " {}, {}, {}", r(*cond), r(args[0]), r(args[1]))
        }
        InstData::StackAddr { slot } => write!(f, " {slot}"),
        InstData::GlobalAddr { global } => {
            f.write_char(' ')?;
            global_name(m, *global, f)
        }
        InstData::FuncAddr { func: callee } => {
            f.write_char(' ')?;
            func_name(m, *callee, f)
        }
        InstData::FieldAddr { base, agg, field } => write!(f, " {}, {agg}, {field}", r(*base)),
        InstData::ElemAddr { base, elem, index } => {
            write!(f, " {}, {elem}, {}", r(*base), r(*index))
        }
        InstData::PtrOffset { base, offset } => write!(f, " {}, {}", r(*base), r(*offset)),
        InstData::Load { addr, flags, .. } => write!(f, " {} {flags}", r(*addr)),
        InstData::Store { value, addr, flags } => write!(f, " {}, {} {flags}", r(*value), r(*addr)),
        InstData::AtomicLoad { addr, order, .. } => write!(f, " {} {order}", r(*addr)),
        InstData::AtomicStore { value, addr, order } => {
            write!(f, " {}, {} {order}", r(*value), r(*addr))
        }
        InstData::AtomicRmw {
            op,
            addr,
            value,
            order,
        } => write!(f, " {} {}, {} {order}", op.name(), r(*addr), r(*value)),
        InstData::CmpXchg {
            addr,
            expected,
            new,
            success,
            failure,
        } => write!(
            f,
            " {}, {}, {} {success} {failure}",
            r(*addr),
            r(*expected),
            r(*new)
        ),
        InstData::Fence { order } => write!(f, " {order}"),
        InstData::MemCopy {
            dst,
            src,
            len,
            flags,
        }
        | InstData::MemMove {
            dst,
            src,
            len,
            flags,
        } => {
            write!(f, " {}, {}, {} {flags}", r(*dst), r(*src), r(*len))
        }
        InstData::MemSet {
            dst,
            byte,
            len,
            flags,
        } => {
            write!(f, " {}, {}, {} {flags}", r(*dst), r(*byte), r(*len))
        }
        InstData::Call(call) => {
            f.write_char(' ')?;
            write_call(f, m, func, call)
        }
        InstData::Safepoint { gc } => {
            f.write_str(" gc(")?;
            write_values(f, func, gc)?;
            f.write_char(')')
        }
    }
}

fn write_results(f: &mut impl Write, func: &Function, results: ValueRange) -> fmt::Result {
    for (i, v) in results.iter().enumerate() {
        if i > 0 {
            f.write_str(", ")?;
        }
        match func.value_type(v) {
            Some(ty) => write!(f, "{v}: {ty}")?,
            None => write!(f, "{v}: ?")?,
        }
    }
    if !results.is_empty() {
        f.write_str(" = ")?;
    }
    Ok(())
}

fn write_terminator(
    f: &mut impl Write,
    m: &Module,
    func: &Function,
    term: &Terminator,
) -> fmt::Result {
    f.write_str(term.opcode())?;
    match term {
        Terminator::Jump(dest) => {
            f.write_char(' ')?;
            write_dest(f, func, dest)
        }
        Terminator::Branch {
            cond,
            then_dest,
            else_dest,
        } => {
            write!(f, " {}, ", func.resolve(*cond))?;
            write_dest(f, func, then_dest)?;
            f.write_str(", ")?;
            write_dest(f, func, else_dest)
        }
        Terminator::Switch {
            value,
            cases,
            default,
        } => {
            write!(f, " {} default ", func.resolve(*value))?;
            write_dest(f, func, default)?;
            f.write_str(" [")?;
            // Case values print as their raw bits (unsigned decimal): the parser
            // may meet a switch before the scrutinee's definition.
            for (i, case) in cases.iter().enumerate() {
                if i > 0 {
                    f.write_str(", ")?;
                }
                write!(f, "{}: ", case.value)?;
                write_dest(f, func, &case.dest)?;
            }
            f.write_char(']')
        }
        Terminator::Return { values } => {
            if !values.is_empty() {
                f.write_char(' ')?;
                write_values(f, func, values)?;
            }
            Ok(())
        }
        Terminator::Unreachable => Ok(()),
        Terminator::Trap { code } => write!(f, " {code}"),
        Terminator::TailCall(call) => {
            f.write_char(' ')?;
            write_call(f, m, func, call)
        }
        Terminator::Invoke {
            call,
            normal,
            unwind,
        } => {
            f.write_char(' ')?;
            write_call(f, m, func, call)?;
            f.write_str(" to ")?;
            write_dest(f, func, normal)?;
            f.write_str(" unwind ")?;
            write_dest(f, func, unwind)
        }
        Terminator::Check { op, normal, error } => {
            f.write_char(' ')?;
            write_op(f, m, func, op, true)?;
            f.write_str(" to ")?;
            write_dest(f, func, normal)?;
            f.write_str(" error ")?;
            write_dest(f, func, error)
        }
        Terminator::Resume { payload } => write!(f, " {}", func.resolve(*payload)),
    }
}

fn write_body(f: &mut impl Write, m: &Module, func: &Function) -> fmt::Result {
    for (i, s) in func.slots.iter().enumerate() {
        writeln!(f, "    ss{i} = slot {} align {}", s.size, s.align)?;
    }
    let entry = func.entry();
    let order = core::iter::once(entry)
        .filter(|&b| func.is_live_block(b))
        .chain(func.blocks().filter(|&b| b != entry));
    for b in order {
        write!(f, "{b}")?;
        let params = func.block_params(b);
        if !params.is_empty() {
            f.write_char('(')?;
            for (i, &p) in params.iter().enumerate() {
                if i > 0 {
                    f.write_str(", ")?;
                }
                match func.value_type(p) {
                    Some(ty) => write!(f, "{p}: {ty}")?,
                    None => write!(f, "{p}: ?")?,
                }
            }
            f.write_char(')')?;
        }
        f.write_str(":\n")?;
        for inst in func.insts(b) {
            f.write_str("    ")?;
            write_results(f, func, func.results(inst))?;
            if let Some(data) = func.inst(inst) {
                write_op(f, m, func, data, false)?;
            }
            f.write_char('\n')?;
        }
        if let Some(term) = func.terminator(b) {
            f.write_str("    ")?;
            write_terminator(f, m, func, term)?;
            f.write_char('\n')?;
        }
    }
    Ok(())
}

fn write_function(f: &mut impl Write, m: &Module, id: FuncId) -> fmt::Result {
    let Some(decl) = m.decls.funcs.get(id.index()) else {
        return Ok(());
    };
    f.write_str("func ")?;
    write_symbol(f, &decl.name)?;
    write!(f, " {} {}", decl.sig, decl.linkage.name())?;
    write_attrs(f, decl.attrs)?;
    match m.function(id) {
        Some(body) => {
            f.write_str(" {\n")?;
            write_body(f, m, body)?;
            f.write_str("}\n")
        }
        None => f.write_char('\n'),
    }
}

/// Prints one function; see [`Module::display_function`].
pub(crate) struct FunctionDisplay<'m> {
    pub(crate) module: &'m Module,
    pub(crate) func: FuncId,
}

impl fmt::Display for FunctionDisplay<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_function(f, self.module, self.func)
    }
}

impl fmt::Display for Module {
    /// Prints the module in the textual form. The output depends only on the module
    /// and is the same on every platform; [`parse`](crate::parse) reads it back.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("module ")?;
        write_quoted(f, &self.name)?;
        let ptr = match self.decls.ptr {
            PointerWidth::P32 => "ptr32",
            PointerWidth::P64 => "ptr64",
        };
        writeln!(f, " {ptr}")?;
        for (i, a) in self.decls.aggs.iter().enumerate() {
            write!(f, "type t{i} = ")?;
            match &a.agg {
                Aggregate::Struct { fields, packed } => {
                    if *packed {
                        f.write_str("packed ")?;
                    }
                    f.write_str("struct {")?;
                    for (k, field) in fields.iter().enumerate() {
                        f.write_str(if k == 0 { " " } else { ", " })?;
                        write!(f, "{field}")?;
                    }
                    f.write_str(" }\n")?;
                }
                Aggregate::Array { elem, len } => writeln!(f, "[{elem} x {len}]")?,
            }
        }
        for (i, s) in self.decls.sigs.iter().enumerate() {
            writeln!(f, "sig s{i} = {s}")?;
        }
        for g in &self.decls.globals {
            let gl = &g.global;
            f.write_str("global ")?;
            write_symbol(f, &g.name)?;
            write!(
                f,
                " {} {} size {} align {}",
                gl.linkage.name(),
                if gl.mutable { "mutable" } else { "const" },
                gl.size,
                gl.align
            )?;
            match &gl.init {
                Init::Zeroed => f.write_str(" zeroed")?,
                Init::Bytes(bytes) => {
                    f.write_str(" bytes \"")?;
                    for b in bytes {
                        write!(f, "{b:02x}")?;
                    }
                    f.write_char('"')?;
                }
            }
            for r in &gl.relocs {
                write!(f, " reloc {} ", r.offset)?;
                match r.target {
                    SymbolRef::Func(func) => func_name(self, func, f)?,
                    SymbolRef::Global(global) => global_name(self, global, f)?,
                }
                write!(f, " {:+}", r.addend)?;
            }
            f.write_char('\n')?;
        }
        for id in self.functions() {
            if self.function(id).is_some() {
                f.write_char('\n')?;
            }
            write_function(f, self, id)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::String;

    #[test]
    fn test_quoting_escapes_specials() {
        let mut s = String::new();
        let _ = write_quoted(&mut s, "a\"b\\c\n\u{1}");
        assert_eq!(s, "\"a\\\"b\\\\c\\n\\u{1}\"");
    }

    #[test]
    fn test_bare_symbols() {
        assert!(is_bare_symbol("main"));
        assert!(is_bare_symbol("_start.x$1"));
        assert!(!is_bare_symbol("1x"));
        assert!(!is_bare_symbol(""));
        assert!(!is_bare_symbol("a b"));
    }

    #[test]
    fn test_const_forms() {
        let mut s = String::new();
        let p = PointerWidth::P64;
        let _ = write_const(&mut s, Type::I8, 0xff, p);
        s.push(' ');
        let _ = write_const(&mut s, Type::U8, 0xff, p);
        s.push(' ');
        let _ = write_const(&mut s, Type::F64, (-0.0f64).to_bits(), p);
        s.push(' ');
        let _ = write_const(&mut s, Type::F64, f64::NAN.to_bits(), p);
        s.push(' ');
        let _ = write_const(&mut s, Type::I8, 0x1ff, p);
        s.push(' ');
        let _ = write_const(&mut s, Type::Ref, 0, p);
        assert_eq!(s, "-1 255 -0.0 nan:0x7ff8000000000000 0x1ff null");
    }
}
