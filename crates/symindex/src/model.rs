//! Whole-repo symbol index data model — deliberately unresolved, name-based,
//! raw syntactic text throughout. A field's or parameter's "type" is
//! whatever token sequence appeared in source (`"int"`, `"*Widget"`,
//! `"List<String>"`), never normalized or resolved against imports. This
//! mirrors `autoreview-archgraph`'s own restraint (package identity is "a
//! plain `String`", not a resolved module) — real type resolution is
//! explicitly deferred future work (a "Tier 4" compiler-frontend backend),
//! not attempted here.

use std::path::PathBuf;

/// One field/parameter name plus its raw declared-type text, exactly as
/// written in source.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NamedSlot {
    pub name: String,
    pub type_text: String,
}

/// A struct (Go) or class (Java) declaration, plus every method whose
/// receiver/enclosing type is this one.
#[derive(Debug, Clone, PartialEq)]
pub struct TypeDecl {
    pub name: String,
    pub file: PathBuf,
    pub start_line: u32,
    pub fields: Vec<NamedSlot>,
    pub methods: Vec<MethodDecl>,
    /// Java only (Go has no class inheritance) — the raw `extends` target's
    /// name, unresolved against imports. `None` for Go types and for Java
    /// classes with no explicit superclass.
    pub superclass: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MethodDecl {
    pub name: String,
    /// The `TypeDecl.name` this method belongs to.
    pub owner_type: String,
    pub file: PathBuf,
    pub start_line: u32,
    pub end_line: u32,
    pub params: Vec<NamedSlot>,
    pub return_type_text: Option<String>,
    pub own_field_accesses: Vec<AccessRef>,
    pub foreign_accesses: Vec<ForeignAccessRef>,
    pub chains: Vec<CallChain>,
    /// True when the method body is empty, or its only statement is a
    /// `throw` whose text mentions `UnsupportedOperationException` or
    /// `NotImplementedError` — the two syntactic shapes Refused Bequest
    /// looks for. Java only.
    pub is_trivial_body: bool,
}

/// A bare identifier or `this.x` (Java) / receiver-name `.x` (Go) reference
/// to one of the enclosing type's own fields, matched by name only against
/// `TypeDecl.fields`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessRef {
    pub field_name: String,
    pub line: u32,
}

/// A `receiver.member` access where `receiver` is one of this method's own
/// **parameters** (locals are deliberately out of scope for this pass — see
/// the plan's Feature Envy scope decision) and `member` isn't a known own
/// field. `receiver_type` is the parameter's declared type text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignAccessRef {
    pub receiver_name: String,
    pub receiver_type: Option<String>,
    pub member_name: String,
    pub line: u32,
}

/// One maximal chain of consecutive `.member`/`.method()` accesses off a
/// single root expression, e.g. `a.getB().getC().getD()` -> depth 3.
/// Root-independent: doesn't matter whether the root is `this`, a param, a
/// local, or a literal — message chains are about the call-site shape, not
/// what the receiver represents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallChain {
    pub root_text: String,
    pub depth: usize,
    pub line: u32,
    pub member_names: Vec<String>,
}

/// The whole-repo index: every `TypeDecl` found across every parsed file.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SymbolIndex {
    pub types: Vec<TypeDecl>,
}

impl SymbolIndex {
    pub fn methods(&self) -> impl Iterator<Item = &MethodDecl> {
        self.types.iter().flat_map(|t| t.methods.iter())
    }

    pub fn find_type(&self, name: &str) -> Option<&TypeDecl> {
        self.types.iter().find(|t| t.name == name)
    }
}
