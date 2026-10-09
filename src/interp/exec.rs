//! The execution engine: an explicit frame stack over validated IR.

use alloc::string::String;
use alloc::vec::Vec;

use super::{
    Host, HostReturn, InterpError, Limits, Memory, NULL_GUARD, NoHost, OpFault, Outcome, Val,
    eval_binary, eval_compare, eval_convert, eval_fma, eval_unary, mask_for, sval,
};
use crate::entity::{Block, FuncId, Inst, Value};
use crate::function::Function;
use crate::inst::{BlockArg, BlockCall, Call, CallTarget, InstData, RmwOp, Terminator};
use crate::module::{Init, Linkage, Module, SymbolRef};
use crate::ops::ConvOp;
use crate::types::{PointerWidth, Type};

/// What a frame is waiting for while its callee runs.
#[derive(Debug)]
enum Pending {
    None,
    /// A `call` instruction: results go to its values.
    Call(Waiting),
    /// The `invoke` terminator of a block.
    Invoke(Waiting, Block),
}

#[derive(Debug)]
struct Waiting {
    inst: Option<Inst>,
    /// Object numbers of the GC list, captured when the call started.
    gc: Vec<u64>,
    safepoint: bool,
    returns: Vec<Type>,
}

#[derive(Debug)]
struct Frame {
    func: FuncId,
    block: Block,
    next: Option<Inst>,
    regs: Vec<u64>,
    slots: Vec<u64>,
    sp_mark: u64,
    pending: Pending,
}

/// A finished callee: its return bits, or an exception payload.
type CallResult = Result<Vec<u64>, u64>;

/// Executes functions of one validated module.
///
/// # Examples
///
/// ```
/// use ir_lang::interp::{InterpError, Interpreter, Limits, NoHost};
/// use ir_lang::{Linkage, Module, Signature};
///
/// let mut m = Module::new("m");
/// let f = m.declare_function("spin", &Signature::new(&[], &[]), Linkage::Export)?;
/// let mut b = m.build(f)?;
/// let l = b.create_block(&[])?;
/// b.jump(l, &[])?;
/// b.switch_to(l)?;
/// b.jump(l, &[])?;
///
/// let mut vm = Interpreter::with_limits(&m, NoHost, Limits { fuel: 100, ..Limits::default() })?;
/// assert_eq!(vm.call(f, &[]), Err(InterpError::FuelExhausted));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug)]
pub struct Interpreter<'m, H: Host> {
    module: &'m Module,
    host: H,
    limits: Limits,
    mem: Memory,
    globals: Vec<u64>,
    fuel: u64,
    frames: Vec<Frame>,
    spare: Vec<Vec<u64>>,
    root_returns: Vec<Type>,
}

/// Runs `func` of a validated `module` with default limits and no host.
///
/// # Errors
///
/// [`InterpError`] if the module does not validate, a budget runs out, or the
/// program does something undefined.
///
/// See the [module documentation](super) for an example.
pub fn run(module: &Module, func: FuncId, args: &[Val]) -> Result<Outcome, InterpError> {
    Interpreter::new(module, NoHost)?.call(func, args)
}

fn call_sig<'a>(module: &'a Module, call: &Call) -> Option<&'a crate::types::Signature> {
    match call.target {
        CallTarget::Direct(f) => module.func_signature(f),
        CallTarget::Indirect { sig, .. } => module.signature(sig),
    }
}

impl<'m, H: Host> Interpreter<'m, H> {
    /// An interpreter with default [`Limits`].
    ///
    /// # Errors
    ///
    /// [`InterpError::Invalid`] if the module does not validate;
    /// [`InterpError::OutOfMemory`] if its globals do not fit.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::interp::{Interpreter, NoHost};
    /// use ir_lang::Module;
    ///
    /// let m = Module::new("empty");
    /// let vm = Interpreter::new(&m, NoHost)?;
    /// assert_eq!(vm.fuel_used(), 0);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn new(module: &'m Module, host: H) -> Result<Self, InterpError> {
        Self::with_limits(module, host, Limits::default())
    }

    /// An interpreter with explicit limits. Globals are laid out and initialized
    /// (relocations included) once, here.
    ///
    /// # Errors
    ///
    /// As [`new`](Interpreter::new).
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::interp::{Interpreter, Limits, NoHost};
    /// use ir_lang::Module;
    ///
    /// let m = Module::new("empty");
    /// let vm = Interpreter::with_limits(&m, NoHost, Limits { memory: 1 << 16, ..Limits::default() })?;
    /// # let _ = vm;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn with_limits(module: &'m Module, host: H, limits: Limits) -> Result<Self, InterpError> {
        module.validate().map_err(InterpError::Invalid)?;
        let mut mem = Memory::new(limits.memory, module.pointer_width())?;
        let mut globals = Vec::with_capacity(module.global_count());
        let mut cursor = NULL_GUARD;
        for g in module.globals() {
            let Some(def) = module.global(g) else {
                continue;
            };
            let align = u64::from(def.align.max(1));
            cursor = cursor
                .checked_add(align - 1)
                .ok_or(InterpError::OutOfMemory)?
                & !(align - 1);
            globals.push(cursor);
            cursor = cursor
                .checked_add(def.size.max(1))
                .ok_or(InterpError::OutOfMemory)?;
        }
        if cursor > mem.size() {
            return Err(InterpError::OutOfMemory);
        }
        mem.grow_low(cursor)?;
        mem.globals_end = cursor;
        mem.stack_base = cursor.checked_add(15).ok_or(InterpError::OutOfMemory)? & !15;
        mem.sp = mem.stack_base;
        if mem.sp > mem.heap_floor {
            return Err(InterpError::OutOfMemory);
        }
        let mut interp = Interpreter {
            module,
            host,
            limits,
            mem,
            globals,
            fuel: 0,
            frames: Vec::new(),
            spare: Vec::new(),
            root_returns: Vec::new(),
        };
        interp.init_globals()?;
        Ok(interp)
    }

    fn code_base(&self) -> u64 {
        match self.module.pointer_width() {
            PointerWidth::P32 => 0xF000_0000,
            PointerWidth::P64 => 0xF000_0000_0000_0000,
        }
    }

    fn func_address(&self, f: FuncId) -> u64 {
        self.code_base() + 16 * (f.index() as u64 + 1)
    }

    fn func_at(&self, addr: u64) -> Option<FuncId> {
        let off = addr.checked_sub(self.code_base())?;
        if off % 16 != 0 || off == 0 {
            return None;
        }
        let index = off / 16 - 1;
        (index < self.module.function_count() as u64).then(|| FuncId::from_u32(index as u32))
    }

    fn init_globals(&mut self) -> Result<(), InterpError> {
        let n = self.module.pointer_width().bytes() as usize;
        for (i, g) in self.module.globals().enumerate() {
            let Some(def) = self.module.global(g) else {
                continue;
            };
            let base = self.globals.get(i).copied().unwrap_or(0);
            if let Init::Bytes(bytes) = &def.init {
                self.mem.write(base, bytes)?;
            }
            for r in &def.relocs {
                let target = match r.target {
                    SymbolRef::Func(f) => self.func_address(f),
                    SymbolRef::Global(t) => self.globals.get(t.index()).copied().unwrap_or(0),
                };
                let le = target.wrapping_add(r.addend as u64).to_le_bytes();
                self.mem
                    .write(base + r.offset, le.get(..n).unwrap_or(&le))?;
            }
        }
        Ok(())
    }

    /// The address space (for inspecting results or preparing inputs).
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::interp::{Interpreter, NoHost};
    /// use ir_lang::{Global, Module};
    ///
    /// let mut m = Module::new("m");
    /// let g = m.declare_global("g", Global::bytes(&[7, 8], 1))?;
    /// let vm = Interpreter::new(&m, NoHost)?;
    /// let addr = vm.global_address(g).unwrap();
    /// assert_eq!(vm.memory().read(addr, 2)?, &[7, 8]);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[must_use]
    pub fn memory(&self) -> &Memory {
        &self.mem
    }

    /// Mutable access to the address space.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::interp::{Interpreter, NoHost};
    /// use ir_lang::{Global, Module};
    ///
    /// let mut m = Module::new("m");
    /// let g = m.declare_global("g", Global::zeroed(4, 4).mutable())?;
    /// let mut vm = Interpreter::new(&m, NoHost)?;
    /// let addr = vm.global_address(g).unwrap();
    /// vm.memory_mut().write(addr, &[1, 2, 3, 4])?;
    /// assert_eq!(vm.memory().read(addr, 4)?, &[1, 2, 3, 4]);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn memory_mut(&mut self) -> &mut Memory {
        &mut self.mem
    }

    /// The address of a global.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::interp::{Interpreter, NoHost};
    /// use ir_lang::{Global, Module};
    ///
    /// let mut m = Module::new("m");
    /// let g = m.declare_global("g", Global::zeroed(8, 8))?;
    /// let vm = Interpreter::new(&m, NoHost)?;
    /// assert_eq!(vm.global_address(g).map(|a| a % 8), Some(0));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[must_use]
    pub fn global_address(&self, global: crate::entity::GlobalId) -> Option<u64> {
        self.globals.get(global.index()).copied()
    }

    /// Fuel used so far, across calls.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::interp::{Interpreter, NoHost};
    /// use ir_lang::{Linkage, Module, Signature};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[], &[]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// b.ret(&[])?;
    /// let mut vm = Interpreter::new(&m, NoHost)?;
    /// vm.call(f, &[])?;
    /// assert_eq!(vm.fuel_used(), 1); // one terminator
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[must_use]
    pub fn fuel_used(&self) -> u64 {
        self.fuel
    }

    /// The host.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::interp::{Interpreter, NoHost};
    /// use ir_lang::Module;
    ///
    /// let m = Module::new("m");
    /// let mut vm = Interpreter::new(&m, NoHost)?;
    /// let _host: &mut NoHost = vm.host_mut();
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn host_mut(&mut self) -> &mut H {
        &mut self.host
    }

    /// Calls `func` with `args` and runs to completion.
    ///
    /// # Errors
    ///
    /// [`InterpError`] if a budget runs out or the program does something
    /// undefined. Memory (globals and the host heap) keeps its contents across
    /// calls; fuel accumulates.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::interp::{Interpreter, NoHost, Outcome, Val};
    /// use ir_lang::{Linkage, Module, Overflow, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("neg", &Signature::new(&[Type::I8], &[Type::I8]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// let x = b.param(0)?;
    /// let zero = b.iconst(Type::I8, 0)?;
    /// let y = b.sub(zero, x, Overflow::Wrap)?;
    /// b.ret(&[y])?;
    /// let mut vm = Interpreter::new(&m, NoHost)?;
    /// assert_eq!(vm.call(f, &[Val::i8(5)])?, Outcome::Return(vec![Val::i8(-5)]));
    /// assert_eq!(vm.call(f, &[Val::i8(-128)])?, Outcome::Return(vec![Val::i8(-128)]));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn call(&mut self, func: FuncId, args: &[Val]) -> Result<Outcome, InterpError> {
        while !self.frames.is_empty() {
            self.pop_frame();
        }
        self.mem.sp = self.mem.stack_base;
        let sig = self
            .module
            .func_signature(func)
            .ok_or(InterpError::BadArguments)?;
        if sig.params.len() != args.len() || sig.params.iter().zip(args).any(|(&t, a)| t != a.ty())
        {
            return Err(InterpError::BadArguments);
        }
        self.root_returns = sig.returns.clone();
        let bits: Vec<u64> = args.iter().map(|a| a.bits()).collect();
        if let Some(outcome) = self.start_call(func, &bits, None)? {
            return Ok(outcome);
        }
        loop {
            if let Some(outcome) = self.step()? {
                return Ok(outcome);
            }
        }
    }

    // ----------------------------------------------------------- frames

    fn top(&self) -> Result<&Frame, InterpError> {
        self.frames.last().ok_or(InterpError::BadArguments)
    }

    fn body_of(&self, f: FuncId) -> Result<&'m Function, InterpError> {
        let module: &'m Module = self.module;
        module
            .function(f)
            .ok_or_else(|| InterpError::UnresolvedImport {
                name: module
                    .func_decl(f)
                    .map_or_else(String::new, |d| d.name().into()),
            })
    }

    fn pop_frame(&mut self) {
        if let Some(f) = self.frames.pop() {
            self.mem.sp = f.sp_mark;
            self.spare.push(f.regs);
        }
    }

    fn push_frame(&mut self, func: FuncId, args: &[u64]) -> Result<(), InterpError> {
        if self.frames.len() >= self.limits.max_depth as usize {
            return Err(InterpError::StackOverflow);
        }
        let body = self.body_of(func)?;
        let mut regs = self.spare.pop().unwrap_or_default();
        regs.clear();
        regs.resize(body.value_count(), 0);
        let entry = body.entry();
        for (&p, &a) in body.block_params(entry).iter().zip(args) {
            if let Some(r) = regs.get_mut(p.index()) {
                *r = a;
            }
        }
        let sp_mark = self.mem.sp;
        let mut slots = Vec::with_capacity(body.slot_count());
        for i in 0..body.slot_count() {
            let Some(s) = body.slot(crate::entity::StackSlot::from_u32(i as u32)) else {
                continue;
            };
            let align = u64::from(s.align.max(1));
            let addr = self
                .mem
                .sp
                .checked_add(align - 1)
                .ok_or(InterpError::OutOfMemory)?
                & !(align - 1);
            let end = addr
                .checked_add(u64::from(s.size).max(1))
                .ok_or(InterpError::OutOfMemory)?;
            if end > self.mem.heap_floor {
                self.mem.sp = sp_mark;
                return Err(InterpError::OutOfMemory);
            }
            if let Err(e) = self.mem.claim_stack(addr, end) {
                self.mem.sp = sp_mark;
                return Err(e);
            }
            self.mem.sp = end;
            slots.push(addr);
        }
        self.frames.push(Frame {
            func,
            block: entry,
            next: body.first_inst(entry),
            regs,
            slots,
            sp_mark,
            pending: Pending::None,
        });
        Ok(())
    }

    /// Starts a call. `pending` (if any) is what the current top frame waits on;
    /// `None` means the callee's result goes to whatever the top frame already
    /// waits on (a tail call, or the root call). Imports run at once through the
    /// host. Returns the final outcome if the stack emptied.
    fn start_call(
        &mut self,
        func: FuncId,
        args: &[u64],
        pending: Option<Pending>,
    ) -> Result<Option<Outcome>, InterpError> {
        if let Some(p) = pending {
            if let Some(top) = self.frames.last_mut() {
                top.pending = p;
            }
        }
        let decl = self
            .module
            .func_decl(func)
            .ok_or(InterpError::BadArguments)?;
        if decl.linkage() != Linkage::Import {
            self.push_frame(func, args)?;
            return Ok(None);
        }
        let sig = self
            .module
            .func_signature(func)
            .ok_or(InterpError::BadArguments)?;
        let vals: Vec<Val> = sig
            .params
            .iter()
            .zip(args)
            .map(|(&ty, &bits)| Val::new(ty, bits))
            .collect();
        let result = match self.host.call(decl.name(), &vals, &mut self.mem)? {
            HostReturn::Values(vs) => {
                if vs.len() != sig.returns.len()
                    || vs.iter().zip(&sig.returns).any(|(v, &t)| v.ty() != t)
                {
                    return Err(InterpError::Host(
                        "host results do not match the signature".into(),
                    ));
                }
                Ok(vs.iter().map(|v| v.bits()).collect())
            }
            HostReturn::Unwind(p) => Err(p.bits()),
        };
        self.deliver(result)
    }

    /// Hands a finished callee's result to the top frame, which is waiting on it,
    /// unwinding through frames that only `call`ed. With no frame left, the result
    /// is the outcome.
    fn deliver(&mut self, mut result: CallResult) -> Result<Option<Outcome>, InterpError> {
        loop {
            let Some(top) = self.frames.last_mut() else {
                return Ok(Some(match result {
                    Ok(rets) => Outcome::Return(
                        rets.iter()
                            .zip(&self.root_returns)
                            .map(|(&bits, &ty)| Val::new(ty, bits))
                            .collect(),
                    ),
                    Err(payload) => Outcome::Unwind(Val::new(Type::Ptr, payload)),
                }));
            };
            let pending = core::mem::replace(&mut top.pending, Pending::None);
            match (pending, result) {
                (Pending::Call(w), Ok(rets)) => {
                    let values = self.relocate(&w, Some(&rets))?;
                    let inst = w.inst.ok_or(InterpError::BadArguments)?;
                    let body = self.body_of(self.top()?.func)?;
                    for (v, bits) in body.results(inst).iter().zip(values) {
                        self.set(v, bits);
                    }
                    return Ok(None);
                }
                (Pending::Invoke(w, block), outcome) => {
                    let body = self.body_of(self.top()?.func)?;
                    let Some(Terminator::Invoke { normal, unwind, .. }) = body.terminator(block)
                    else {
                        return Err(InterpError::BadArguments);
                    };
                    let (dest, values, exn) = match outcome {
                        Ok(rets) => (normal, self.relocate(&w, Some(&rets))?, 0),
                        Err(payload) => (unwind, self.relocate(&w, None)?, payload),
                    };
                    self.take_edge(body, dest, &values, exn, 0)?;
                    return Ok(None);
                }
                (Pending::Call(_) | Pending::None, Err(payload)) => {
                    // A plain call propagates the exception out of this frame.
                    self.pop_frame();
                    result = Err(payload);
                }
                (Pending::None, Ok(_)) => return Err(InterpError::BadArguments),
            }
        }
    }

    /// The results a finished call defines: its returns (zeros on the unwind
    /// path), then the relocated GC list. At a safepoint, returned references are
    /// checked against the callee's epoch and re-minted in a new one.
    fn relocate(&mut self, w: &Waiting, rets: Option<&[u64]>) -> Result<Vec<u64>, InterpError> {
        let mut out = Vec::with_capacity(w.returns.len() + w.gc.len());
        let mut refs = Vec::new();
        for (i, &ty) in w.returns.iter().enumerate() {
            let bits = rets.and_then(|r| r.get(i).copied()).unwrap_or(0);
            if w.safepoint && ty == Type::Ref && rets.is_some() {
                refs.push((i, self.mem.decode_ref(bits)?));
            }
            out.push(bits);
        }
        if w.safepoint {
            self.mem.new_epoch();
        }
        for (i, index) in refs {
            if let Some(slot) = out.get_mut(i) {
                *slot = self.mem.encode_ref(index);
            }
        }
        out.extend(w.gc.iter().map(|&i| self.mem.encode_ref(i)));
        Ok(out)
    }

    // ------------------------------------------------------------- stepping

    fn reg(&self, body: &Function, v: Value) -> u64 {
        let r = body.resolve(v);
        self.frames
            .last()
            .and_then(|f| f.regs.get(r.index()).copied())
            .unwrap_or(0)
    }

    fn ty(body: &Function, v: Value) -> Type {
        body.value_type(body.resolve(v)).unwrap_or(Type::U64)
    }

    fn set(&mut self, v: Value, bits: u64) {
        if let Some(r) = self
            .frames
            .last_mut()
            .and_then(|f| f.regs.get_mut(v.index()))
        {
            *r = bits;
        }
    }

    fn burn(&mut self, units: u64) -> Result<(), InterpError> {
        self.fuel = self.fuel.saturating_add(units);
        if self.fuel > self.limits.fuel {
            Err(InterpError::FuelExhausted)
        } else {
            Ok(())
        }
    }

    /// Executes one instruction or terminator of the top frame.
    fn step(&mut self) -> Result<Option<Outcome>, InterpError> {
        self.burn(1)?;
        let (func, block, next) = {
            let top = self.top()?;
            (top.func, top.block, top.next)
        };
        let body = self.body_of(func)?;
        match next {
            Some(inst) => {
                if let Some(top) = self.frames.last_mut() {
                    top.next = body.next_inst(inst);
                }
                self.exec_inst(body, inst)
            }
            None => self.exec_term(body, block),
        }
    }

    /// Takes an edge: evaluates every argument, then assigns the parameters at
    /// once (a parallel copy).
    fn take_edge(
        &mut self,
        body: &Function,
        dest: &BlockCall,
        results: &[u64],
        exn: u64,
        err: u64,
    ) -> Result<(), InterpError> {
        let args: Vec<u64> = dest
            .args
            .iter()
            .map(|a| match *a {
                BlockArg::Value(v) => self.reg(body, v),
                BlockArg::Result(i) => results.get(i as usize).copied().unwrap_or(0),
                BlockArg::Exception => exn,
                BlockArg::ErrorCode => err,
            })
            .collect();
        for (&p, bits) in body.block_params(dest.block).iter().zip(args) {
            self.set(p, bits);
        }
        let top = self.frames.last_mut().ok_or(InterpError::BadArguments)?;
        top.block = dest.block;
        top.next = body.first_inst(dest.block);
        Ok(())
    }

    fn aligned(addr: u64, align: u64) -> Result<(), InterpError> {
        if align > 1 && addr % align != 0 {
            Err(InterpError::Misaligned { addr, align })
        } else {
            Ok(())
        }
    }

    fn ptr_mask(&self) -> u64 {
        mask_for(Type::Ptr, self.module.pointer_width())
    }

    fn eval_op(
        &self,
        body: &Function,
        data: &InstData,
    ) -> Result<Result<u64, OpFault>, InterpError> {
        Ok(match data {
            InstData::Unary { op, policy, arg } => {
                eval_unary(*op, *policy, Self::ty(body, *arg), self.reg(body, *arg))
            }
            InstData::Binary { op, policy, args } => eval_binary(
                *op,
                *policy,
                Self::ty(body, args[0]),
                self.reg(body, args[0]),
                self.reg(body, args[1]),
            ),
            InstData::Convert {
                op,
                policy,
                to,
                arg,
            } => {
                let from = Self::ty(body, *arg);
                let a = self.reg(body, *arg);
                if *op == ConvOp::RefToPtr {
                    Ok(self.mem.ref_address(Val::new(Type::Ref, a))?)
                } else {
                    let to_mask = mask_for(*to, self.module.pointer_width());
                    eval_convert(*op, *policy, from, *to, a).map(|b| b & to_mask)
                }
            }
            _ => return Err(InterpError::BadArguments),
        })
    }

    fn call_target(&self, body: &Function, call: &Call) -> Result<FuncId, InterpError> {
        match call.target {
            CallTarget::Direct(f) => Ok(f),
            CallTarget::Indirect { sig, callee } => {
                let addr = self.reg(body, callee);
                let f = self
                    .func_at(addr)
                    .ok_or(InterpError::BadFunctionPointer { addr })?;
                let want = self.module.signature(sig);
                if want.is_none() || self.module.func_signature(f) != want {
                    return Err(InterpError::BadFunctionPointer { addr });
                }
                Ok(f)
            }
        }
    }

    fn waiting(
        &self,
        body: &Function,
        call: &Call,
        inst: Option<Inst>,
    ) -> Result<Waiting, InterpError> {
        let mut gc = Vec::with_capacity(call.gc.len());
        for &g in &call.gc {
            gc.push(self.mem.decode_ref(self.reg(body, g))?);
        }
        Ok(Waiting {
            inst,
            gc,
            safepoint: crate::typing::call_is_safepoint(&self.module.decls, call),
            returns: call_sig(self.module, call)
                .map(|s| s.returns.clone())
                .unwrap_or_default(),
        })
    }

    fn put(&mut self, results: crate::entity::ValueRange, bits: u64) {
        if let Some(v) = results.first() {
            self.set(v, bits);
        }
    }

    fn exec_inst(
        &mut self,
        body: &'m Function,
        inst: Inst,
    ) -> Result<Option<Outcome>, InterpError> {
        let Some(data) = body.inst(inst) else {
            return Err(InterpError::BadArguments);
        };
        let results = body.results(inst);
        let ptr = self.module.pointer_width();
        let r = |this: &Self, v: Value| this.reg(body, v);
        match data {
            InstData::Const { bits, .. } => self.put(results, *bits),
            InstData::Unary { .. } | InstData::Binary { .. } | InstData::Convert { .. } => {
                match self.eval_op(body, data)? {
                    Ok(bits) => self.put(results, bits),
                    Err(OpFault::Trap(kind)) => return Ok(Some(self.trap(kind.code()))),
                    Err(OpFault::Error(_)) => return Err(InterpError::UncheckedError),
                }
            }
            InstData::Fma { args } => {
                let bits = eval_fma(
                    Self::ty(body, args[0]),
                    r(self, args[0]),
                    r(self, args[1]),
                    r(self, args[2]),
                );
                self.put(results, bits);
            }
            InstData::Compare { op, args } => {
                let ty = Self::ty(body, args[0]);
                let (a, b) = (r(self, args[0]), r(self, args[1]));
                let (a, b) = if ty == Type::Ref {
                    (self.mem.decode_ref(a)?, self.mem.decode_ref(b)?)
                } else {
                    (a, b)
                };
                self.put(results, u64::from(eval_compare(*op, ty, a, b)));
            }
            InstData::Select { cond, args } => {
                let bits = if r(self, *cond) != 0 {
                    r(self, args[0])
                } else {
                    r(self, args[1])
                };
                self.put(results, bits);
            }
            InstData::StackAddr { slot } => {
                let addr = self.top()?.slots.get(slot.index()).copied().unwrap_or(0);
                self.put(results, addr);
            }
            InstData::GlobalAddr { global } => {
                let addr = self.globals.get(global.index()).copied().unwrap_or(0);
                self.put(results, addr);
            }
            InstData::FuncAddr { func } => {
                let addr = self.func_address(*func) & self.ptr_mask();
                self.put(results, addr);
            }
            InstData::FieldAddr { base, agg, field } => {
                let off = self.module.field_offset(*agg, *field).unwrap_or(0);
                let addr = r(self, *base).wrapping_add(off) & self.ptr_mask();
                self.put(results, addr);
            }
            InstData::ElemAddr { base, elem, index } => {
                let size = self.module.layout(*elem).map_or(0, |l| l.size());
                let i = sval(Self::ty(body, *index), r(self, *index));
                let off = i.wrapping_mul(i128::from(size)) as u64;
                let addr = r(self, *base).wrapping_add(off) & self.ptr_mask();
                self.put(results, addr);
            }
            InstData::PtrOffset { base, offset } => {
                let off = sval(Self::ty(body, *offset), r(self, *offset)) as u64;
                let addr = r(self, *base).wrapping_add(off) & self.ptr_mask();
                self.put(results, addr);
            }
            InstData::Load { ty, addr, flags } => {
                let a = r(self, *addr);
                Self::aligned(a, u64::from(flags.align))?;
                let bits = self.mem.load(*ty, a)?;
                self.put(results, bits);
            }
            InstData::Store { value, addr, flags } => {
                let a = r(self, *addr);
                Self::aligned(a, u64::from(flags.align))?;
                self.mem.store(Self::ty(body, *value), a, r(self, *value))?;
            }
            InstData::AtomicLoad { ty, addr, .. } => {
                let a = r(self, *addr);
                Self::aligned(a, u64::from(ty.size(ptr)))?;
                let bits = self.mem.load(*ty, a)?;
                self.put(results, bits);
            }
            InstData::AtomicStore { value, addr, .. } => {
                let ty = Self::ty(body, *value);
                let a = r(self, *addr);
                Self::aligned(a, u64::from(ty.size(ptr)))?;
                self.mem.store(ty, a, r(self, *value))?;
            }
            InstData::AtomicRmw {
                op, addr, value, ..
            } => {
                let ty = Self::ty(body, *value);
                let a = r(self, *addr);
                Self::aligned(a, u64::from(ty.size(ptr)))?;
                let old = self.mem.load(ty, a)?;
                let v = r(self, *value);
                let m = mask_for(ty, ptr);
                let new = match op {
                    RmwOp::Xchg => v,
                    RmwOp::Add => old.wrapping_add(v) & m,
                    RmwOp::Sub => old.wrapping_sub(v) & m,
                    RmwOp::And => old & v,
                    RmwOp::Or => old | v,
                    RmwOp::Xor => old ^ v,
                    RmwOp::Nand => !(old & v) & m,
                    RmwOp::Min => {
                        if sval(ty, old) <= sval(ty, v) {
                            old
                        } else {
                            v
                        }
                    }
                    RmwOp::Max => {
                        if sval(ty, old) >= sval(ty, v) {
                            old
                        } else {
                            v
                        }
                    }
                };
                self.mem.store(ty, a, new)?;
                self.put(results, old);
            }
            InstData::CmpXchg {
                addr,
                expected,
                new,
                ..
            } => {
                let ty = Self::ty(body, *expected);
                let a = r(self, *addr);
                Self::aligned(a, u64::from(ty.size(ptr)))?;
                let old = self.mem.load(ty, a)?;
                let hit = old == r(self, *expected);
                if hit {
                    self.mem.store(ty, a, r(self, *new))?;
                }
                if let Some(v) = results.get(0) {
                    self.set(v, old);
                }
                if let Some(v) = results.get(1) {
                    self.set(v, u64::from(hit));
                }
            }
            InstData::Fence { .. } => {}
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
                let (d, s, n) = (r(self, *dst), r(self, *src), r(self, *len));
                self.burn(n / 64)?;
                Self::aligned(d, u64::from(flags.align))?;
                Self::aligned(s, u64::from(flags.align))?;
                let overlap = n > 0 && d < s.saturating_add(n) && s < d.saturating_add(n);
                if overlap && matches!(data, InstData::MemCopy { .. }) {
                    return Err(InterpError::Overlap);
                }
                let bytes = self.mem.read(s, n)?.to_vec();
                self.mem.write(d, &bytes)?;
            }
            InstData::MemSet {
                dst,
                byte,
                len,
                flags,
            } => {
                let (d, b, n) = (r(self, *dst), r(self, *byte), r(self, *len));
                self.burn(n / 64)?;
                Self::aligned(d, u64::from(flags.align))?;
                // Check the range before allocating the fill buffer.
                let _ = self.mem.read(d, n)?;
                let fill = alloc::vec![b as u8; n as usize];
                self.mem.write(d, &fill)?;
            }
            InstData::Call(call) => {
                let target = self.call_target(body, call)?;
                let args: Vec<u64> = call.args.iter().map(|&a| r(self, a)).collect();
                let w = self.waiting(body, call, Some(inst))?;
                return self.start_call(target, &args, Some(Pending::Call(w)));
            }
            InstData::Safepoint { gc } => {
                let mut indices = Vec::with_capacity(gc.len());
                for &g in gc.iter() {
                    indices.push(self.mem.decode_ref(r(self, g))?);
                }
                self.mem.new_epoch();
                for (v, i) in results.iter().zip(indices) {
                    let bits = self.mem.encode_ref(i);
                    self.set(v, bits);
                }
            }
        }
        Ok(None)
    }

    fn trap(&mut self, code: u32) -> Outcome {
        while !self.frames.is_empty() {
            self.pop_frame();
        }
        Outcome::Trap(code)
    }

    fn exec_term(
        &mut self,
        body: &'m Function,
        block: Block,
    ) -> Result<Option<Outcome>, InterpError> {
        let Some(term) = body.terminator(block) else {
            return Err(InterpError::UnreachableReached);
        };
        match term {
            Terminator::Jump(dest) => self.take_edge(body, dest, &[], 0, 0)?,
            Terminator::Branch {
                cond,
                then_dest,
                else_dest,
            } => {
                let dest = if self.reg(body, *cond) != 0 {
                    then_dest
                } else {
                    else_dest
                };
                self.take_edge(body, dest, &[], 0, 0)?;
            }
            Terminator::Switch {
                value,
                cases,
                default,
            } => {
                let v = self.reg(body, *value);
                let dest = cases
                    .iter()
                    .find(|c| c.value == v)
                    .map_or(default, |c| &c.dest);
                self.take_edge(body, dest, &[], 0, 0)?;
            }
            Terminator::Return { values } => {
                let rets: Vec<u64> = values.iter().map(|&v| self.reg(body, v)).collect();
                self.pop_frame();
                return self.deliver(Ok(rets));
            }
            Terminator::Unreachable => return Err(InterpError::UnreachableReached),
            Terminator::Trap { code } => return Ok(Some(self.trap(*code))),
            Terminator::TailCall(call) => {
                let target = self.call_target(body, call)?;
                let args: Vec<u64> = call.args.iter().map(|&a| self.reg(body, a)).collect();
                self.pop_frame();
                return self.start_call(target, &args, None);
            }
            Terminator::Invoke { call, .. } => {
                let target = self.call_target(body, call)?;
                let args: Vec<u64> = call.args.iter().map(|&a| self.reg(body, a)).collect();
                let w = self.waiting(body, call, None)?;
                return self.start_call(target, &args, Some(Pending::Invoke(w, block)));
            }
            Terminator::Check { op, normal, error } => match self.eval_op(body, op)? {
                Ok(bits) => self.take_edge(body, normal, &[bits], 0, 0)?,
                Err(OpFault::Error(kind)) => {
                    self.take_edge(body, error, &[], 0, u64::from(kind.code()))?;
                }
                Err(OpFault::Trap(kind)) => return Ok(Some(self.trap(kind.code()))),
            },
            Terminator::Resume { payload } => {
                let p = self.reg(body, *payload);
                self.pop_frame();
                return self.deliver(Err(p));
            }
        }
        Ok(None)
    }
}
