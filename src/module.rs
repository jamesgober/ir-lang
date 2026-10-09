//! The [`Module`]: functions, globals, signatures, and aggregates.

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::builder::Builder;
use crate::entity::{AggId, FuncId, GlobalId, SigId};
use crate::error::{ModuleError, ValidationError};
use crate::function::Function;
use crate::inst::Attrs;
use crate::types::{Aggregate, Layout, MAX_AGGREGATE_SIZE, MemType, PointerWidth, Signature};

/// How a symbol is visible outside its module.
///
/// # Examples
///
/// ```
/// use ir_lang::Linkage;
///
/// assert_eq!(Linkage::Export.name(), "export");
/// assert_eq!(Linkage::from_name("import"), Some(Linkage::Import));
/// ```
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[non_exhaustive]
pub enum Linkage {
    /// Defined here and visible only inside the module.
    Local,
    /// Defined here and visible to other modules.
    Export,
    /// Declared here and defined elsewhere (another module, the runtime, the host,
    /// or the platform).
    Import,
}

impl Linkage {
    /// The textual name.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::Linkage;
    ///
    /// assert_eq!(Linkage::Local.name(), "local");
    /// ```
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Linkage::Local => "local",
            Linkage::Export => "export",
            Linkage::Import => "import",
        }
    }

    /// Looks a linkage up by its textual name.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::Linkage;
    ///
    /// assert_eq!(Linkage::from_name("export"), Some(Linkage::Export));
    /// ```
    #[must_use]
    pub fn from_name(name: &str) -> Option<Linkage> {
        match name {
            "local" => Some(Linkage::Local),
            "export" => Some(Linkage::Export),
            "import" => Some(Linkage::Import),
            _ => None,
        }
    }
}

/// A function declaration: name, signature, linkage, and attributes.
///
/// # Examples
///
/// ```
/// use ir_lang::{Attrs, Linkage, Module, Signature, Type};
///
/// let mut m = Module::new("m");
/// let f = m.declare_function("puts", &Signature::new(&[Type::Ptr], &[Type::I32]), Linkage::Import)?;
/// m.set_attrs(f, Attrs::NOGC)?;
/// let d = m.func_decl(f).unwrap();
/// assert_eq!((d.name(), d.linkage(), d.attrs()), ("puts", Linkage::Import, Attrs::NOGC));
/// assert_eq!(m.signature(d.sig()).unwrap().returns, [Type::I32]);
/// # Ok::<(), ir_lang::ModuleError>(())
/// ```
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FuncDecl {
    pub(crate) name: String,
    pub(crate) sig: SigId,
    pub(crate) linkage: Linkage,
    pub(crate) attrs: Attrs,
}

impl FuncDecl {
    /// The symbol name.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("main", &Signature::new(&[], &[]), Linkage::Export)?;
    /// assert_eq!(m.func_decl(f).unwrap().name(), "main");
    /// # Ok::<(), ir_lang::ModuleError>(())
    /// ```
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The signature.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[], &[]), Linkage::Export)?;
    /// let s = m.func_decl(f).unwrap().sig();
    /// assert_eq!(m.signature(s), Some(&Signature::new(&[], &[])));
    /// # Ok::<(), ir_lang::ModuleError>(())
    /// ```
    #[must_use]
    pub fn sig(&self) -> SigId {
        self.sig
    }

    /// The linkage.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[], &[]), Linkage::Import)?;
    /// assert_eq!(m.func_decl(f).unwrap().linkage(), Linkage::Import);
    /// # Ok::<(), ir_lang::ModuleError>(())
    /// ```
    #[must_use]
    pub fn linkage(&self) -> Linkage {
        self.linkage
    }

    /// The attributes.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Attrs, Linkage, Module, Signature};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[], &[]), Linkage::Import)?;
    /// assert_eq!(m.func_decl(f).unwrap().attrs(), Attrs::NONE);
    /// # Ok::<(), ir_lang::ModuleError>(())
    /// ```
    #[must_use]
    pub fn attrs(&self) -> Attrs {
        self.attrs
    }
}

/// What a relocation points at.
///
/// # Examples
///
/// ```
/// use ir_lang::{FuncId, SymbolRef};
///
/// assert!(matches!(SymbolRef::Func(FuncId::from_u32(0)), SymbolRef::Func(_)));
/// ```
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[non_exhaustive]
pub enum SymbolRef {
    /// A function's address.
    Func(FuncId),
    /// A global's address.
    Global(GlobalId),
}

/// An address stored inside a global's initial data: at `offset`, the address of
/// `target` plus `addend`, as a pointer-width little-endian word.
///
/// # Examples
///
/// ```
/// use ir_lang::{GlobalId, Reloc, SymbolRef};
///
/// let r = Reloc { offset: 8, target: SymbolRef::Global(GlobalId::from_u32(0)), addend: 4 };
/// assert_eq!(r.offset, 8);
/// ```
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Reloc {
    /// Byte offset inside the global.
    pub offset: u64,
    /// The symbol whose address is stored.
    pub target: SymbolRef,
    /// Added to the address.
    pub addend: i64,
}

/// The initial contents of a global.
///
/// # Examples
///
/// ```
/// use ir_lang::Init;
///
/// assert_eq!(Init::Bytes(vec![1, 2]), Init::Bytes(vec![1, 2]));
/// ```
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
#[non_exhaustive]
pub enum Init {
    /// All zero bytes.
    Zeroed,
    /// Exactly `size` bytes.
    Bytes(Vec<u8>),
}

/// A global (data) definition or import.
///
/// Little-endian byte order is assumed for initializers and relocations (every
/// target the family supports is little-endian).
///
/// # Examples
///
/// ```
/// use ir_lang::{Global, Linkage, Module};
///
/// let mut m = Module::new("m");
/// let g = m.declare_global("counter", Global::zeroed(8, 8).mutable())?;
/// let msg = m.declare_global("msg", Global::bytes(b"hi\0", 1).with_linkage(Linkage::Local))?;
/// assert!(m.global(g).unwrap().mutable);
/// assert_eq!(m.global(msg).unwrap().size, 3);
/// # Ok::<(), ir_lang::ModuleError>(())
/// ```
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Global {
    /// Linkage. An `Import` has `Init::Zeroed` and no relocations.
    pub linkage: Linkage,
    /// Size in bytes.
    pub size: u64,
    /// Alignment in bytes (a power of two, at most 2^16).
    pub align: u32,
    /// Whether the program may write it.
    pub mutable: bool,
    /// Initial contents.
    pub init: Init,
    /// Addresses stored in the initial contents.
    pub relocs: Vec<Reloc>,
}

impl Global {
    /// The largest alignment a global may request.
    pub const MAX_ALIGN: u32 = 1 << 16;

    /// An exported, immutable, zero-initialized global.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Global, Init};
    ///
    /// assert_eq!(Global::zeroed(4, 4).init, Init::Zeroed);
    /// ```
    #[must_use]
    pub fn zeroed(size: u64, align: u32) -> Global {
        Global {
            linkage: Linkage::Export,
            size,
            align,
            mutable: false,
            init: Init::Zeroed,
            relocs: Vec::new(),
        }
    }

    /// An exported, immutable global holding `bytes`.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::Global;
    ///
    /// assert_eq!(Global::bytes(&[1, 2, 3], 1).size, 3);
    /// ```
    #[must_use]
    pub fn bytes(bytes: &[u8], align: u32) -> Global {
        Global {
            linkage: Linkage::Export,
            size: bytes.len() as u64,
            align,
            mutable: false,
            init: Init::Bytes(bytes.to_vec()),
            relocs: Vec::new(),
        }
    }

    /// An imported global of the given size.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Global, Linkage};
    ///
    /// assert_eq!(Global::import(8, 8).linkage, Linkage::Import);
    /// ```
    #[must_use]
    pub fn import(size: u64, align: u32) -> Global {
        Global {
            linkage: Linkage::Import,
            ..Global::zeroed(size, align)
        }
    }

    /// The same global, writable.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::Global;
    ///
    /// assert!(Global::zeroed(1, 1).mutable().mutable);
    /// ```
    #[must_use]
    pub fn mutable(mut self) -> Global {
        self.mutable = true;
        self
    }

    /// The same global with another linkage.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Global, Linkage};
    ///
    /// assert_eq!(Global::zeroed(1, 1).with_linkage(Linkage::Local).linkage, Linkage::Local);
    /// ```
    #[must_use]
    pub fn with_linkage(mut self, linkage: Linkage) -> Global {
        self.linkage = linkage;
        self
    }

    /// The same global with a relocation added.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{FuncId, Global, Reloc, SymbolRef};
    ///
    /// let g = Global::zeroed(8, 8).with_reloc(Reloc { offset: 0, target: SymbolRef::Func(FuncId::from_u32(0)), addend: 0 });
    /// assert_eq!(g.relocs.len(), 1);
    /// ```
    #[must_use]
    pub fn with_reloc(mut self, reloc: Reloc) -> Global {
        self.relocs.push(reloc);
        self
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct AggData {
    pub(crate) agg: Aggregate,
    pub(crate) layout: Layout,
    pub(crate) offsets: Vec<u64>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct GlobalDecl {
    pub(crate) name: String,
    pub(crate) global: Global,
}

/// The declarations of a module, borrowed by a builder while it edits one body.
#[derive(Clone, Debug, Default)]
pub(crate) struct Decls {
    pub(crate) ptr: PointerWidth,
    pub(crate) sigs: Vec<Signature>,
    pub(crate) sig_index: BTreeMap<Signature, SigId>,
    pub(crate) aggs: Vec<AggData>,
    pub(crate) funcs: Vec<FuncDecl>,
    pub(crate) globals: Vec<GlobalDecl>,
}

impl Decls {
    pub(crate) fn layout(&self, ty: MemType) -> Option<Layout> {
        match ty {
            MemType::Scalar(t) => {
                let size = t.size(self.ptr);
                Some(Layout {
                    size: u64::from(size),
                    align: size.max(1),
                })
            }
            MemType::Agg(a) => self.aggs.get(a.index()).map(|d| d.layout),
        }
    }

    pub(crate) fn func_sig(&self, func: FuncId) -> Option<&Signature> {
        self.funcs
            .get(func.index())
            .and_then(|d| self.sigs.get(d.sig.index()))
    }

    /// Computes the layout and field offsets of an aggregate whose components must
    /// all be declared before `limit`.
    pub(crate) fn compute_layout(
        &self,
        agg: &Aggregate,
        limit: usize,
    ) -> Result<(Layout, Vec<u64>), ModuleError> {
        let component = |m: MemType| -> Result<Layout, ModuleError> {
            if let MemType::Agg(a) = m {
                if a.index() >= limit {
                    return Err(ModuleError::UnknownAggregate { agg: a });
                }
            }
            self.layout(m).ok_or(ModuleError::AggregateTooLarge)
        };
        match agg {
            Aggregate::Struct { fields, packed } => {
                let mut offset = 0u64;
                let mut align = 1u32;
                let mut offsets = Vec::with_capacity(fields.len());
                for &field in fields {
                    let l = component(field)?;
                    if !packed {
                        offset = round_up(offset, l.align).ok_or(ModuleError::AggregateTooLarge)?;
                        align = align.max(l.align);
                    }
                    offsets.push(offset);
                    offset = offset
                        .checked_add(l.size)
                        .filter(|&s| s <= MAX_AGGREGATE_SIZE)
                        .ok_or(ModuleError::AggregateTooLarge)?;
                }
                let size = if *packed {
                    offset
                } else {
                    round_up(offset, align).ok_or(ModuleError::AggregateTooLarge)?
                };
                if size > MAX_AGGREGATE_SIZE {
                    return Err(ModuleError::AggregateTooLarge);
                }
                Ok((Layout { size, align }, offsets))
            }
            Aggregate::Array { elem, len } => {
                let l = component(*elem)?;
                let size = l
                    .size
                    .checked_mul(*len)
                    .filter(|&s| s <= MAX_AGGREGATE_SIZE)
                    .ok_or(ModuleError::AggregateTooLarge)?;
                Ok((
                    Layout {
                        size,
                        align: l.align,
                    },
                    Vec::new(),
                ))
            }
        }
    }
}

pub(crate) fn round_up(value: u64, align: u32) -> Option<u64> {
    let a = u64::from(align.max(1));
    let rem = value % a;
    if rem == 0 {
        Some(value)
    } else {
        value.checked_add(a - rem)
    }
}

pub(crate) fn valid_align(align: u64, max: u64) -> bool {
    align != 0 && align.is_power_of_two() && align <= max
}

/// A compilation unit of IR: function declarations and bodies, globals, interned
/// signatures, and aggregate types.
///
/// Declarations come first (so calls and addresses can name any function or global);
/// bodies are then built with [`build`](Module::build) and changed with
/// [`edit`](Module::edit). [`validate`](Module::validate) checks the whole module, the
/// [`Display`](core::fmt::Display) implementation prints it in the textual form, and
/// [`parse`](crate::parse) reads that form back.
///
/// # Examples
///
/// ```
/// use ir_lang::{CmpOp, Linkage, Module, Signature, Type};
///
/// // fn max(a: i64, b: i64) -> i64 { if a < b { b } else { a } }
/// let mut m = Module::new("demo");
/// let f = m.declare_function("max", &Signature::new(&[Type::I64, Type::I64], &[Type::I64]), Linkage::Export)?;
/// let mut b = m.build(f)?;
/// let (a, c) = (b.param(0)?, b.param(1)?);
/// let lt = b.compare(CmpOp::Lt, a, c)?;
/// let exit = b.create_block(&[Type::I64])?;
/// b.branch(lt, exit, &[c], exit, &[a])?;
/// b.switch_to(exit)?;
/// let r = b.block_param(exit, 0)?;
/// b.ret(&[r])?;
///
/// m.validate()?;
/// let text = m.to_string();
/// assert!(text.contains("br v2, b1(v1), b1(v0)"));
/// assert_eq!(ir_lang::parse(&text)?.to_string(), text);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Debug)]
pub struct Module {
    pub(crate) name: String,
    pub(crate) decls: Decls,
    pub(crate) bodies: Vec<Option<Function>>,
    pub(crate) names: BTreeMap<String, SymbolRef>,
}

impl Module {
    /// An empty module with 64-bit addresses.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Module, PointerWidth};
    ///
    /// let m = Module::new("app");
    /// assert_eq!((m.name(), m.pointer_width()), ("app", PointerWidth::P64));
    /// ```
    #[must_use]
    pub fn new(name: &str) -> Module {
        Module::with_pointer_width(name, PointerWidth::P64)
    }

    /// An empty module with the given address width.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Module, PointerWidth, Type};
    ///
    /// let m = Module::with_pointer_width("wasm", PointerWidth::P32);
    /// assert_eq!(m.layout(Type::Ptr.into()).unwrap().size(), 4);
    /// ```
    #[must_use]
    pub fn with_pointer_width(name: &str, ptr: PointerWidth) -> Module {
        Module {
            name: name.to_string(),
            decls: Decls {
                ptr,
                ..Decls::default()
            },
            bodies: Vec::new(),
            names: BTreeMap::new(),
        }
    }

    /// The module's name.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::Module;
    ///
    /// assert_eq!(Module::new("app").name(), "app");
    /// ```
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The address width.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Module, PointerWidth};
    ///
    /// assert_eq!(Module::with_pointer_width("w", PointerWidth::P32).pointer_width(), PointerWidth::P32);
    /// ```
    #[must_use]
    pub fn pointer_width(&self) -> PointerWidth {
        self.decls.ptr
    }

    /// Interns a signature, returning the existing handle for an equal one.
    ///
    /// # Errors
    ///
    /// [`ModuleError::CapacityExceeded`] if the table is full.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Module, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let a = m.intern_signature(&Signature::new(&[Type::I32], &[]))?;
    /// let b = m.intern_signature(&Signature::new(&[Type::I32], &[]))?;
    /// assert_eq!(a, b);
    /// # Ok::<(), ir_lang::ModuleError>(())
    /// ```
    pub fn intern_signature(&mut self, sig: &Signature) -> Result<SigId, ModuleError> {
        if let Some(&id) = self.decls.sig_index.get(sig) {
            return Ok(id);
        }
        let id = SigId::from_u32(next_index(self.decls.sigs.len())?);
        self.decls.sigs.push(sig.clone());
        let _ = self.decls.sig_index.insert(sig.clone(), id);
        Ok(id)
    }

    /// The signature behind a handle.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Module, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let s = m.intern_signature(&Signature::new(&[], &[Type::Bool]))?;
    /// assert_eq!(m.signature(s).unwrap().returns, [Type::Bool]);
    /// # Ok::<(), ir_lang::ModuleError>(())
    /// ```
    #[must_use]
    pub fn signature(&self, sig: SigId) -> Option<&Signature> {
        self.decls.sigs.get(sig.index())
    }

    /// The number of interned signatures.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Module, Signature};
    ///
    /// let mut m = Module::new("m");
    /// m.intern_signature(&Signature::new(&[], &[]))?;
    /// m.intern_signature(&Signature::new(&[], &[]))?;
    /// assert_eq!(m.signature_count(), 1);
    /// # Ok::<(), ir_lang::ModuleError>(())
    /// ```
    #[must_use]
    pub fn signature_count(&self) -> usize {
        self.decls.sigs.len()
    }

    /// Declares an aggregate type and computes its layout.
    ///
    /// # Errors
    ///
    /// [`ModuleError::UnknownAggregate`] if it names an aggregate not declared
    /// before it; [`ModuleError::AggregateTooLarge`] if its size exceeds
    /// [`MAX_AGGREGATE_SIZE`]; [`ModuleError::CapacityExceeded`].
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Aggregate, Module, Type};
    ///
    /// let mut m = Module::new("m");
    /// let v3 = m.declare_aggregate(Aggregate::array(Type::F32.into(), 3))?;
    /// assert_eq!(m.layout(v3.into()).unwrap().size(), 12);
    /// # Ok::<(), ir_lang::ModuleError>(())
    /// ```
    pub fn declare_aggregate(&mut self, agg: Aggregate) -> Result<AggId, ModuleError> {
        let index = next_index(self.decls.aggs.len())?;
        let (layout, offsets) = self.decls.compute_layout(&agg, index as usize)?;
        self.decls.aggs.push(AggData {
            agg,
            layout,
            offsets,
        });
        Ok(AggId::from_u32(index))
    }

    /// The aggregate behind a handle.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Aggregate, Module, Type};
    ///
    /// let mut m = Module::new("m");
    /// let a = m.declare_aggregate(Aggregate::array(Type::U8.into(), 3))?;
    /// assert_eq!(m.aggregate(a), Some(&Aggregate::array(Type::U8.into(), 3)));
    /// # Ok::<(), ir_lang::ModuleError>(())
    /// ```
    #[must_use]
    pub fn aggregate(&self, agg: AggId) -> Option<&Aggregate> {
        self.decls.aggs.get(agg.index()).map(|d| &d.agg)
    }

    /// The number of declared aggregates.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Aggregate, Module, Type};
    ///
    /// let mut m = Module::new("m");
    /// m.declare_aggregate(Aggregate::structure(&[Type::U8.into()]))?;
    /// assert_eq!(m.aggregate_count(), 1);
    /// # Ok::<(), ir_lang::ModuleError>(())
    /// ```
    #[must_use]
    pub fn aggregate_count(&self) -> usize {
        self.decls.aggs.len()
    }

    /// The size and alignment of a memory type, or `None` for an unknown
    /// aggregate.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Module, Type};
    ///
    /// let m = Module::new("m");
    /// assert_eq!(m.layout(Type::Bool.into()).unwrap().size(), 1);
    /// ```
    #[must_use]
    pub fn layout(&self, ty: MemType) -> Option<Layout> {
        self.decls.layout(ty)
    }

    /// The byte offset of a struct field, or `None` if `agg` is not a struct or has
    /// no such field.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Aggregate, Module, Type};
    ///
    /// let mut m = Module::new("m");
    /// let s = m.declare_aggregate(Aggregate::packed(&[Type::U8.into(), Type::U32.into()]))?;
    /// assert_eq!(m.field_offset(s, 1), Some(1));
    /// assert_eq!(m.field_offset(s, 2), None);
    /// # Ok::<(), ir_lang::ModuleError>(())
    /// ```
    #[must_use]
    pub fn field_offset(&self, agg: AggId, field: u32) -> Option<u64> {
        let data = self.decls.aggs.get(agg.index())?;
        match data.agg {
            Aggregate::Struct { .. } => data.offsets.get(field as usize).copied(),
            Aggregate::Array { .. } => None,
        }
    }

    fn claim_name(&self, name: &str) -> Result<(), ModuleError> {
        if name.is_empty() {
            return Err(ModuleError::EmptyName);
        }
        if self.names.contains_key(name) {
            return Err(ModuleError::DuplicateName {
                name: name.to_string(),
            });
        }
        Ok(())
    }

    /// Declares a function. Its signature is interned. A `Local` or `Export`
    /// function needs a body ([`build`](Module::build)) before the module validates;
    /// an `Import` must not have one.
    ///
    /// # Errors
    ///
    /// [`ModuleError::EmptyName`], [`ModuleError::DuplicateName`],
    /// [`ModuleError::CapacityExceeded`].
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("main", &Signature::new(&[], &[Type::I32]), Linkage::Export)?;
    /// assert_eq!(m.func_decl(f).unwrap().name(), "main");
    /// # Ok::<(), ir_lang::ModuleError>(())
    /// ```
    pub fn declare_function(
        &mut self,
        name: &str,
        sig: &Signature,
        linkage: Linkage,
    ) -> Result<FuncId, ModuleError> {
        self.claim_name(name)?;
        let index = next_index(self.decls.funcs.len())?;
        let sig = self.intern_signature(sig)?;
        let id = FuncId::from_u32(index);
        self.decls.funcs.push(FuncDecl {
            name: name.to_string(),
            sig,
            linkage,
            attrs: Attrs::NONE,
        });
        self.bodies.push(None);
        let _ = self.names.insert(name.to_string(), SymbolRef::Func(id));
        Ok(id)
    }

    /// Sets a function's attributes.
    ///
    /// # Errors
    ///
    /// [`ModuleError::UnknownFunction`].
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Attrs, Linkage, Module, Signature};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("abort", &Signature::new(&[], &[]), Linkage::Import)?;
    /// m.set_attrs(f, Attrs::NORETURN | Attrs::NOUNWIND)?;
    /// assert!(m.func_decl(f).unwrap().attrs().contains(Attrs::NORETURN));
    /// # Ok::<(), ir_lang::ModuleError>(())
    /// ```
    pub fn set_attrs(&mut self, func: FuncId, attrs: Attrs) -> Result<(), ModuleError> {
        let decl = self
            .decls
            .funcs
            .get_mut(func.index())
            .ok_or(ModuleError::UnknownFunction { func })?;
        decl.attrs = attrs;
        Ok(())
    }

    /// A function's declaration.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[], &[]), Linkage::Local)?;
    /// assert_eq!(m.func_decl(f).map(|d| d.linkage()), Some(Linkage::Local));
    /// # Ok::<(), ir_lang::ModuleError>(())
    /// ```
    #[must_use]
    pub fn func_decl(&self, func: FuncId) -> Option<&FuncDecl> {
        self.decls.funcs.get(func.index())
    }

    /// A function's signature.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[Type::U8], &[]), Linkage::Import)?;
    /// assert_eq!(m.func_signature(f).unwrap().params, [Type::U8]);
    /// # Ok::<(), ir_lang::ModuleError>(())
    /// ```
    #[must_use]
    pub fn func_signature(&self, func: FuncId) -> Option<&Signature> {
        self.decls.func_sig(func)
    }

    /// The number of declared functions.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature};
    ///
    /// let mut m = Module::new("m");
    /// m.declare_function("f", &Signature::new(&[], &[]), Linkage::Import)?;
    /// assert_eq!(m.function_count(), 1);
    /// # Ok::<(), ir_lang::ModuleError>(())
    /// ```
    #[must_use]
    pub fn function_count(&self) -> usize {
        self.decls.funcs.len()
    }

    /// Iterates over every declared function, in handle order.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[], &[]), Linkage::Import)?;
    /// assert_eq!(m.functions().collect::<Vec<_>>(), [f]);
    /// # Ok::<(), ir_lang::ModuleError>(())
    /// ```
    pub fn functions(&self) -> impl ExactSizeIterator<Item = FuncId> {
        (0..self.decls.funcs.len() as u32).map(FuncId::from_u32)
    }

    /// A function's body, if it has one.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[], &[]), Linkage::Export)?;
    /// assert!(m.function(f).is_none());
    /// let mut b = m.build(f)?;
    /// b.ret(&[])?;
    /// assert!(m.function(f).is_some());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[must_use]
    pub fn function(&self, func: FuncId) -> Option<&Function> {
        self.bodies.get(func.index()).and_then(Option::as_ref)
    }

    /// Declares a global.
    ///
    /// # Errors
    ///
    /// [`ModuleError::EmptyName`], [`ModuleError::DuplicateName`],
    /// [`ModuleError::BadAlignment`], [`ModuleError::InitSizeMismatch`],
    /// [`ModuleError::RelocOutOfRange`], [`ModuleError::ImportWithData`],
    /// [`ModuleError::CapacityExceeded`]. Relocation targets are checked by
    /// [`validate`](Module::validate), so a global may point at symbols declared
    /// after it (or at itself).
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Global, Module, ModuleError};
    ///
    /// let mut m = Module::new("m");
    /// let err = m.declare_global("bad", Global::zeroed(8, 3)).unwrap_err();
    /// assert_eq!(err, ModuleError::BadAlignment { align: 3 });
    /// ```
    pub fn declare_global(&mut self, name: &str, global: Global) -> Result<GlobalId, ModuleError> {
        self.claim_name(name)?;
        check_global(&global, self.decls.ptr)?;
        let index = next_index(self.decls.globals.len())?;
        let id = GlobalId::from_u32(index);
        self.decls.globals.push(GlobalDecl {
            name: name.to_string(),
            global,
        });
        let _ = self.names.insert(name.to_string(), SymbolRef::Global(id));
        Ok(id)
    }

    /// A global's definition.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Global, Module};
    ///
    /// let mut m = Module::new("m");
    /// let g = m.declare_global("g", Global::zeroed(16, 8))?;
    /// assert_eq!(m.global(g).map(|g| g.size), Some(16));
    /// # Ok::<(), ir_lang::ModuleError>(())
    /// ```
    #[must_use]
    pub fn global(&self, global: GlobalId) -> Option<&Global> {
        self.decls.globals.get(global.index()).map(|g| &g.global)
    }

    /// A global's name.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Global, Module};
    ///
    /// let mut m = Module::new("m");
    /// let g = m.declare_global("table", Global::zeroed(8, 8))?;
    /// assert_eq!(m.global_name(g), Some("table"));
    /// # Ok::<(), ir_lang::ModuleError>(())
    /// ```
    #[must_use]
    pub fn global_name(&self, global: GlobalId) -> Option<&str> {
        self.decls
            .globals
            .get(global.index())
            .map(|g| g.name.as_str())
    }

    /// The number of declared globals.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Global, Module};
    ///
    /// let mut m = Module::new("m");
    /// m.declare_global("g", Global::zeroed(1, 1))?;
    /// assert_eq!(m.global_count(), 1);
    /// # Ok::<(), ir_lang::ModuleError>(())
    /// ```
    #[must_use]
    pub fn global_count(&self) -> usize {
        self.decls.globals.len()
    }

    /// Iterates over every declared global, in handle order.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Global, Module};
    ///
    /// let mut m = Module::new("m");
    /// let g = m.declare_global("g", Global::zeroed(1, 1))?;
    /// assert_eq!(m.globals().collect::<Vec<_>>(), [g]);
    /// # Ok::<(), ir_lang::ModuleError>(())
    /// ```
    pub fn globals(&self) -> impl ExactSizeIterator<Item = GlobalId> {
        (0..self.decls.globals.len() as u32).map(GlobalId::from_u32)
    }

    /// Looks a function or global up by name.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Global, Module, SymbolRef};
    ///
    /// let mut m = Module::new("m");
    /// let g = m.declare_global("g", Global::zeroed(1, 1))?;
    /// assert_eq!(m.lookup("g"), Some(SymbolRef::Global(g)));
    /// assert_eq!(m.lookup("h"), None);
    /// # Ok::<(), ir_lang::ModuleError>(())
    /// ```
    #[must_use]
    pub fn lookup(&self, name: &str) -> Option<SymbolRef> {
        self.names.get(name).copied()
    }

    /// Creates the body of a declared function and returns a [`Builder`]
    /// positioned at the end of its entry block. The entry block's parameters are
    /// the signature's parameters.
    ///
    /// # Errors
    ///
    /// [`ModuleError::UnknownFunction`], [`ModuleError::CannotDefineImport`],
    /// [`ModuleError::AlreadyDefined`].
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[], &[]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// b.ret(&[])?;
    /// assert!(m.build(f).is_err());
    /// m.validate()?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn build(&mut self, func: FuncId) -> Result<Builder<'_>, ModuleError> {
        let decl = self
            .decls
            .funcs
            .get(func.index())
            .ok_or(ModuleError::UnknownFunction { func })?;
        if decl.linkage == Linkage::Import {
            return Err(ModuleError::CannotDefineImport { func });
        }
        let params = self
            .decls
            .sigs
            .get(decl.sig.index())
            .map(|s| s.params.clone())
            .unwrap_or_default();
        let slot = self
            .bodies
            .get_mut(func.index())
            .ok_or(ModuleError::UnknownFunction { func })?;
        if slot.is_some() {
            return Err(ModuleError::AlreadyDefined { func });
        }
        let body = slot.insert(Function::new(&params));
        Ok(Builder::new(&self.decls, body, func))
    }

    /// Returns a [`Builder`] over an existing body, positioned at the end of the
    /// entry block, for rewriting.
    ///
    /// # Errors
    ///
    /// [`ModuleError::UnknownFunction`], [`ModuleError::NotDefined`].
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[], &[]), Linkage::Export)?;
    /// let _ = m.build(f)?;
    /// let mut b = m.edit(f)?;
    /// b.ret(&[])?;
    /// m.validate()?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn edit(&mut self, func: FuncId) -> Result<Builder<'_>, ModuleError> {
        let body = self
            .bodies
            .get_mut(func.index())
            .ok_or(ModuleError::UnknownFunction { func })?
            .as_mut()
            .ok_or(ModuleError::NotDefined { func })?;
        Ok(Builder::new(&self.decls, body, func))
    }

    /// Checks the whole module: declarations, then every function body.
    ///
    /// Validation is total: it either accepts the module, guaranteeing every rule
    /// of `specs/IR.md` holds, or returns the first violation. It never panics and
    /// runs in time linear in the size of the module (plus the size of the live
    /// ranges of GC references, for the safepoint rule).
    ///
    /// # Errors
    ///
    /// The first [`ValidationError`] found.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[], &[]), Linkage::Export)?;
    /// assert!(m.validate().is_err()); // no body yet
    /// let mut b = m.build(f)?;
    /// b.ret(&[])?;
    /// assert!(m.validate().is_ok());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn validate(&self) -> Result<(), ValidationError> {
        crate::validate::validate_module(self)
    }

    /// Checks one function body (and the declarations it uses).
    ///
    /// # Errors
    ///
    /// The first [`ValidationError`] found in that function;
    /// [`ValidationErrorKind::UnknownFunction`](crate::ValidationErrorKind::UnknownFunction)
    /// or [`MissingBody`](crate::ValidationErrorKind::MissingBody) if there is no
    /// such body.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[], &[]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// b.ret(&[])?;
    /// m.validate_function(f)?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn validate_function(&self, func: FuncId) -> Result<(), ValidationError> {
        crate::validate::validate_one(self, func)
    }

    /// Prints one function in the textual form.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[], &[]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// b.ret(&[])?;
    /// assert_eq!(m.display_function(f).to_string(), "func @f s0 export {\nb0:\n    ret\n}\n");
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[must_use]
    pub fn display_function(&self, func: FuncId) -> impl core::fmt::Display + '_ {
        crate::print::FunctionDisplay { module: self, func }
    }

    /// Installs a body without checks (the parser's door; validation catches any
    /// inconsistency).
    pub(crate) fn set_body(&mut self, func: FuncId, body: Function) {
        if let Some(slot) = self.bodies.get_mut(func.index()) {
            *slot = Some(body);
        }
    }
}

fn next_index(len: usize) -> Result<u32, ModuleError> {
    u32::try_from(len)
        .ok()
        .filter(|&i| i < crate::function::MAX_ENTITIES)
        .ok_or(ModuleError::CapacityExceeded)
}

pub(crate) fn check_global(global: &Global, ptr: PointerWidth) -> Result<(), ModuleError> {
    if !valid_align(u64::from(global.align), u64::from(Global::MAX_ALIGN)) {
        return Err(ModuleError::BadAlignment {
            align: u64::from(global.align),
        });
    }
    if global.linkage == Linkage::Import
        && (global.init != Init::Zeroed || !global.relocs.is_empty())
    {
        return Err(ModuleError::ImportWithData);
    }
    if let Init::Bytes(bytes) = &global.init {
        if bytes.len() as u64 != global.size {
            return Err(ModuleError::InitSizeMismatch {
                size: global.size,
                found: bytes.len() as u64,
            });
        }
    }
    for r in &global.relocs {
        let fits = r
            .offset
            .checked_add(u64::from(ptr.bytes()))
            .is_some_and(|end| end <= global.size);
        if !fits {
            return Err(ModuleError::RelocOutOfRange { offset: r.offset });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Type;

    #[test]
    fn test_struct_layout_follows_c_rules() {
        let mut m = Module::new("m");
        let s = m
            .declare_aggregate(Aggregate::structure(&[
                Type::U8.into(),
                Type::U32.into(),
                Type::U16.into(),
            ]))
            .unwrap_or(AggId::from_u32(0));
        let l = m.layout(s.into()).unwrap_or(Layout { size: 0, align: 0 });
        assert_eq!((l.size, l.align), (12, 4));
        assert_eq!(m.field_offset(s, 0), Some(0));
        assert_eq!(m.field_offset(s, 1), Some(4));
        assert_eq!(m.field_offset(s, 2), Some(8));
    }

    #[test]
    fn test_aggregate_cannot_name_a_later_aggregate() {
        let mut m = Module::new("m");
        let err = m.declare_aggregate(Aggregate::array(AggId::from_u32(0).into(), 2));
        assert_eq!(
            err,
            Err(ModuleError::UnknownAggregate {
                agg: AggId::from_u32(0)
            })
        );
    }

    #[test]
    fn test_huge_aggregate_is_refused() {
        let mut m = Module::new("m");
        let err = m.declare_aggregate(Aggregate::array(Type::U64.into(), u64::MAX / 4));
        assert_eq!(err, Err(ModuleError::AggregateTooLarge));
    }

    #[test]
    fn test_global_checks() {
        let mut m = Module::new("m");
        assert_eq!(
            m.declare_global(
                "x",
                Global {
                    init: Init::Bytes(alloc::vec![1]),
                    ..Global::zeroed(2, 1)
                }
            ),
            Err(ModuleError::InitSizeMismatch { size: 2, found: 1 })
        );
        let reloc = Reloc {
            offset: 4,
            target: SymbolRef::Global(GlobalId::from_u32(0)),
            addend: 0,
        };
        assert_eq!(
            m.declare_global("y", Global::zeroed(8, 8).with_reloc(reloc)),
            Err(ModuleError::RelocOutOfRange { offset: 4 })
        );
        assert_eq!(
            m.declare_global("", Global::zeroed(8, 8)),
            Err(ModuleError::EmptyName)
        );
    }
}
