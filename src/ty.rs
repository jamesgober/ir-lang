//! The machine-level value types an IR value can have.

use core::fmt;

/// The type of a value in the IR.
///
/// This is the IR's own *machine-level* type system, deliberately small and
/// independent of any source language. A front-end lowering its program decides
/// how its source types map onto these — a 32-bit and a 64-bit source integer both
/// lower to [`Type::Int`] here, a source `bool` to [`Type::Bool`], and a value that
/// produces nothing (a statement, a `void` call) to [`Type::Unit`]. The validator
/// uses these types to reject operations applied to the wrong kind of value, so the
/// set is kept to the four cases an arithmetic-and-control-flow core actually needs.
///
/// Wider machine types (sized integers, vectors, pointers) are a deliberate later
/// addition: a new variant is an additive, non-breaking change.
///
/// # Examples
///
/// ```
/// use ir_lang::Type;
///
/// // Types are small, `Copy`, and print as their lowercase name.
/// assert_eq!(Type::Int.to_string(), "int");
/// assert_eq!(Type::Bool.to_string(), "bool");
/// assert_ne!(Type::Int, Type::Float);
/// ```
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Type {
    /// A signed integer value.
    Int,
    /// A floating-point value.
    Float,
    /// A boolean value, as produced by a comparison or a logical operation.
    Bool,
    /// The absence of a value — the result type of an operation that exists only
    /// for its effect, and the return type of a function that returns nothing.
    Unit,
}

impl Type {
    /// Returns `true` for the numeric types ([`Int`](Type::Int) and
    /// [`Float`](Type::Float)) that arithmetic and ordering operations accept.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::Type;
    ///
    /// assert!(Type::Int.is_numeric());
    /// assert!(Type::Float.is_numeric());
    /// assert!(!Type::Bool.is_numeric());
    /// assert!(!Type::Unit.is_numeric());
    /// ```
    #[must_use]
    pub const fn is_numeric(self) -> bool {
        matches!(self, Type::Int | Type::Float)
    }
}

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Type::Int => "int",
            Type::Float => "float",
            Type::Bool => "bool",
            Type::Unit => "unit",
        };
        f.write_str(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_type_is_numeric_classifies_each_variant() {
        assert!(Type::Int.is_numeric());
        assert!(Type::Float.is_numeric());
        assert!(!Type::Bool.is_numeric());
        assert!(!Type::Unit.is_numeric());
    }

    #[test]
    fn test_type_display_matches_lowercase_name() {
        assert_eq!(Type::Int.to_string(), "int");
        assert_eq!(Type::Float.to_string(), "float");
        assert_eq!(Type::Bool.to_string(), "bool");
        assert_eq!(Type::Unit.to_string(), "unit");
    }
}
