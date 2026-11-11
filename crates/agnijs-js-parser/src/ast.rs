//! The abstract syntax tree. Shaped after ESTree so the compiler and the
//! tooling can lean on a familiar vocabulary, with a [`Span`] on every node.

use crate::span::Span;
use std::rc::Rc;

/// An identifier (binding or reference). Names are interned per parse by
/// sharing the `Rc`.
#[derive(Clone, Debug, PartialEq)]
pub struct Ident {
    pub name: Rc<str>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PrivateName {
    pub name: Rc<str>,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceType {
    Script,
    Module,
}

#[derive(Debug)]
pub struct Program {
    pub source_type: SourceType,
    pub body: Vec<Statement>,
    /// The program (or its directive prologue) is strict.
    pub strict: bool,
    pub span: Span,
}

// ---- statements ---------------------------------------------------------

#[derive(Debug)]
pub enum Statement {
    Block(Block),
    Empty(Span),
    Expression(ExpressionStatement),
    If(Box<IfStatement>),
    Labeled(Box<LabeledStatement>),
    Break(JumpStatement),
    Continue(JumpStatement),
    With(Box<WithStatement>),
    Switch(Box<SwitchStatement>),
    Return(ReturnStatement),
    Throw(ThrowStatement),
    Try(Box<TryStatement>),
    While(Box<WhileStatement>),
    DoWhile(Box<DoWhileStatement>),
    For(Box<ForStatement>),
    ForIn(Box<ForInStatement>),
    ForOf(Box<ForOfStatement>),
    Debugger(Span),
    /// `var`, `let`, `const` (and `using` is not in ES2025).
    VariableDeclaration(VariableDeclaration),
    FunctionDeclaration(Box<Function>),
    ClassDeclaration(Box<Class>),
    Import(Box<ImportDeclaration>),
    ExportNamed(Box<ExportNamedDeclaration>),
    ExportDefault(Box<ExportDefaultDeclaration>),
    ExportAll(Box<ExportAllDeclaration>),
}

impl Statement {
    pub fn span(&self) -> Span {
        match self {
            Statement::Block(b) => b.span,
            Statement::Empty(s) | Statement::Debugger(s) => *s,
            Statement::Expression(e) => e.span,
            Statement::If(s) => s.span,
            Statement::Labeled(s) => s.span,
            Statement::Break(s) | Statement::Continue(s) => s.span,
            Statement::With(s) => s.span,
            Statement::Switch(s) => s.span,
            Statement::Return(s) => s.span,
            Statement::Throw(s) => s.span,
            Statement::Try(s) => s.span,
            Statement::While(s) => s.span,
            Statement::DoWhile(s) => s.span,
            Statement::For(s) => s.span,
            Statement::ForIn(s) => s.span,
            Statement::ForOf(s) => s.span,
            Statement::VariableDeclaration(d) => d.span,
            Statement::FunctionDeclaration(f) => f.span,
            Statement::ClassDeclaration(c) => c.span,
            Statement::Import(d) => d.span,
            Statement::ExportNamed(d) => d.span,
            Statement::ExportDefault(d) => d.span,
            Statement::ExportAll(d) => d.span,
        }
    }
}

#[derive(Debug)]
pub struct Block {
    pub body: Vec<Statement>,
    pub span: Span,
}

#[derive(Debug)]
pub struct ExpressionStatement {
    pub expression: Expression,
    /// Set when this is a string-literal directive in a prologue.
    pub directive: Option<Box<str>>,
    pub span: Span,
}

#[derive(Debug)]
pub struct IfStatement {
    pub test: Expression,
    pub consequent: Statement,
    pub alternate: Option<Statement>,
    pub span: Span,
}

#[derive(Debug)]
pub struct LabeledStatement {
    pub label: Ident,
    pub body: Statement,
    pub span: Span,
}

#[derive(Debug)]
pub struct JumpStatement {
    pub label: Option<Ident>,
    pub span: Span,
}

#[derive(Debug)]
pub struct WithStatement {
    pub object: Expression,
    pub body: Statement,
    pub span: Span,
}

#[derive(Debug)]
pub struct SwitchStatement {
    pub discriminant: Expression,
    pub cases: Vec<SwitchCase>,
    pub span: Span,
}

#[derive(Debug)]
pub struct SwitchCase {
    /// `None` for `default:`.
    pub test: Option<Expression>,
    pub consequent: Vec<Statement>,
    pub span: Span,
}

#[derive(Debug)]
pub struct ReturnStatement {
    pub argument: Option<Expression>,
    pub span: Span,
}

#[derive(Debug)]
pub struct ThrowStatement {
    pub argument: Expression,
    pub span: Span,
}

#[derive(Debug)]
pub struct TryStatement {
    pub block: Block,
    pub handler: Option<CatchClause>,
    pub finalizer: Option<Block>,
    pub span: Span,
}

#[derive(Debug)]
pub struct CatchClause {
    pub param: Option<Pattern>,
    pub body: Block,
    pub span: Span,
}

#[derive(Debug)]
pub struct WhileStatement {
    pub test: Expression,
    pub body: Statement,
    pub span: Span,
}

#[derive(Debug)]
pub struct DoWhileStatement {
    pub body: Statement,
    pub test: Expression,
    pub span: Span,
}

#[derive(Debug)]
pub enum ForInit {
    VariableDeclaration(VariableDeclaration),
    Expression(Expression),
}

#[derive(Debug)]
pub struct ForStatement {
    pub init: Option<ForInit>,
    pub test: Option<Expression>,
    pub update: Option<Expression>,
    pub body: Statement,
    pub span: Span,
}

#[derive(Debug)]
pub enum ForLeft {
    VariableDeclaration(VariableDeclaration),
    Pattern(Pattern),
}

#[derive(Debug)]
pub struct ForInStatement {
    pub left: ForLeft,
    pub right: Expression,
    pub body: Statement,
    pub span: Span,
}

#[derive(Debug)]
pub struct ForOfStatement {
    pub left: ForLeft,
    pub right: Expression,
    pub body: Statement,
    pub is_await: bool,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VariableKind {
    Var,
    Let,
    Const,
    /// `using` (explicit resource management).
    Using,
    /// `await using`.
    AwaitUsing,
}

impl VariableKind {
    /// Whether the bindings are immutable (const, using, await using).
    pub fn is_const(self) -> bool {
        matches!(self, Self::Const | Self::Using | Self::AwaitUsing)
    }

    /// Whether this is a `using` or `await using` declaration.
    pub fn is_using(self) -> bool {
        matches!(self, Self::Using | Self::AwaitUsing)
    }
}

#[derive(Debug)]
pub struct VariableDeclaration {
    pub kind: VariableKind,
    pub declarations: Vec<VariableDeclarator>,
    pub span: Span,
}

#[derive(Debug)]
pub struct VariableDeclarator {
    pub id: Pattern,
    pub init: Option<Expression>,
    pub span: Span,
}

// ---- patterns -----------------------------------------------------------

#[derive(Debug)]
pub enum Pattern {
    Ident(Ident),
    Object(Box<ObjectPattern>),
    Array(Box<ArrayPattern>),
    /// Only in assignment patterns: any simple assignment target.
    Expression(Box<Expression>),
    Assignment(Box<AssignmentPattern>),
    Rest(Box<RestElement>),
}

impl Pattern {
    pub fn span(&self) -> Span {
        match self {
            Pattern::Ident(i) => i.span,
            Pattern::Object(o) => o.span,
            Pattern::Array(a) => a.span,
            Pattern::Expression(e) => e.span(),
            Pattern::Assignment(a) => a.span,
            Pattern::Rest(r) => r.span,
        }
    }
}

#[derive(Debug)]
pub struct ObjectPattern {
    pub properties: Vec<ObjectPatternProperty>,
    pub rest: Option<Pattern>,
    pub span: Span,
}

#[derive(Debug)]
pub struct ObjectPatternProperty {
    pub key: PropertyKey,
    pub value: Pattern,
    pub shorthand: bool,
    pub span: Span,
}

#[derive(Debug)]
pub struct ArrayPattern {
    /// `None` for holes.
    pub elements: Vec<Option<Pattern>>,
    pub span: Span,
}

#[derive(Debug)]
pub struct AssignmentPattern {
    pub left: Pattern,
    pub right: Expression,
    pub span: Span,
}

#[derive(Debug)]
pub struct RestElement {
    pub argument: Pattern,
    pub span: Span,
}

// ---- expressions --------------------------------------------------------

#[derive(Debug)]
pub enum Expression {
    Ident(Ident),
    This(Span),
    Super(Span),
    Null(Span),
    Bool(bool, Span),
    Number(f64, Span),
    BigInt(Box<str>, Span),
    String(StringLiteral),
    RegExp(Box<RegExpLiteral>),
    Template(Box<TemplateLiteral>),
    TaggedTemplate(Box<TaggedTemplate>),
    Array(Box<ArrayExpression>),
    Object(Box<ObjectExpression>),
    Function(Box<Function>),
    Arrow(Box<ArrowFunction>),
    Class(Box<Class>),
    Unary(Box<UnaryExpression>),
    Update(Box<UpdateExpression>),
    Binary(Box<BinaryExpression>),
    Logical(Box<LogicalExpression>),
    Assignment(Box<AssignmentExpression>),
    Conditional(Box<ConditionalExpression>),
    Call(Box<CallExpression>),
    New(Box<NewExpression>),
    Member(Box<MemberExpression>),
    /// `a?.b`, `a?.[b]`, `a?.()`: the chain's root with `optional` set on
    /// the links that short-circuit.
    OptionalChain(Box<Expression>, Span),
    Sequence(Box<SequenceExpression>),
    Yield(Box<YieldExpression>),
    Await(Box<AwaitExpression>),
    MetaProperty(MetaProperty),
    Import(Box<ImportCall>),
    Spread(Box<SpreadElement>),
    /// `#x in obj`.
    PrivateIn(Box<PrivateInExpression>),
    /// A parenthesised expression (kept so cover grammars and `delete (x)`
    /// can tell).
    Paren(Box<Expression>, Span),
}

impl Expression {
    pub fn span(&self) -> Span {
        match self {
            Expression::Ident(i) => i.span,
            Expression::This(s) | Expression::Super(s) | Expression::Null(s) => *s,
            Expression::Bool(_, s) | Expression::Number(_, s) | Expression::BigInt(_, s) => *s,
            Expression::String(s) => s.span,
            Expression::RegExp(r) => r.span,
            Expression::Template(t) => t.span,
            Expression::TaggedTemplate(t) => t.span,
            Expression::Array(a) => a.span,
            Expression::Object(o) => o.span,
            Expression::Function(f) => f.span,
            Expression::Arrow(a) => a.span,
            Expression::Class(c) => c.span,
            Expression::Unary(u) => u.span,
            Expression::Update(u) => u.span,
            Expression::Binary(b) => b.span,
            Expression::Logical(l) => l.span,
            Expression::Assignment(a) => a.span,
            Expression::Conditional(c) => c.span,
            Expression::Call(c) => c.span,
            Expression::New(n) => n.span,
            Expression::Member(m) => m.span,
            Expression::OptionalChain(_, s) => *s,
            Expression::Sequence(s) => s.span,
            Expression::Yield(y) => y.span,
            Expression::Await(a) => a.span,
            Expression::MetaProperty(m) => m.span,
            Expression::Import(i) => i.span,
            Expression::Spread(s) => s.span,
            Expression::PrivateIn(p) => p.span,
            Expression::Paren(_, s) => *s,
        }
    }

    /// Strip parentheses.
    pub fn unparen(&self) -> &Expression {
        let mut e = self;
        while let Expression::Paren(inner, _) = e {
            e = inner;
        }
        e
    }
}

#[derive(Debug, Clone)]
pub struct StringLiteral {
    pub value: Rc<[u16]>,
    pub span: Span,
}

#[derive(Debug)]
pub struct RegExpLiteral {
    /// UTF-16 code units: a pattern may hold lone surrogates.
    pub pattern: Box<[u16]>,
    pub flags: Box<str>,
    pub span: Span,
}

#[derive(Debug)]
pub struct TemplateLiteral {
    /// `quasis.len() == expressions.len() + 1`.
    pub quasis: Vec<TemplateElement>,
    pub expressions: Vec<Expression>,
    pub span: Span,
}

#[derive(Debug)]
pub struct TemplateElement {
    pub cooked: Option<Rc<[u16]>>,
    pub raw: Rc<[u16]>,
    pub span: Span,
}

#[derive(Debug)]
pub struct TaggedTemplate {
    pub tag: Expression,
    pub quasi: TemplateLiteral,
    pub span: Span,
}

#[derive(Debug)]
pub struct ArrayExpression {
    /// `None` for holes; spread elements are `Expression::Spread`.
    pub elements: Vec<Option<Expression>>,
    /// A comma followed the last element and that element was a spread:
    /// invalid once the literal becomes a destructuring pattern.
    pub trailing_comma_after_spread: bool,
    pub span: Span,
}

#[derive(Debug)]
pub struct ObjectExpression {
    pub properties: Vec<ObjectProperty>,
    /// `{...a,}`: fine as a literal, invalid as a pattern.
    pub trailing_comma_after_spread: bool,
    pub span: Span,
}

#[derive(Debug)]
pub enum ObjectProperty {
    Property(Property),
    Spread(SpreadElement),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PropertyKind {
    Init,
    Get,
    Set,
    Method,
}

#[derive(Debug)]
pub struct Property {
    pub key: PropertyKey,
    pub value: Expression,
    pub kind: PropertyKind,
    pub shorthand: bool,
    /// `__proto__: v` (not computed, not shorthand): sets the prototype.
    pub is_proto_setter: bool,
    pub span: Span,
}

#[derive(Debug)]
pub enum PropertyKey {
    Ident(Ident),
    String(StringLiteral),
    Number(f64, Span),
    BigInt(Box<str>, Span),
    Computed(Box<Expression>),
    Private(PrivateName),
}

impl PropertyKey {
    pub fn span(&self) -> Span {
        match self {
            PropertyKey::Ident(i) => i.span,
            PropertyKey::String(s) => s.span,
            PropertyKey::Number(_, s) | PropertyKey::BigInt(_, s) => *s,
            PropertyKey::Computed(e) => e.span(),
            PropertyKey::Private(p) => p.span,
        }
    }

    /// The static name of a non-computed, non-private key as UTF-16.
    pub fn static_name(&self) -> Option<Vec<u16>> {
        match self {
            PropertyKey::Ident(i) => Some(i.name.encode_utf16().collect()),
            PropertyKey::String(s) => Some(s.value.to_vec()),
            PropertyKey::Number(n, _) => {
                Some(crate::number::to_js_string(*n).encode_utf16().collect())
            }
            PropertyKey::BigInt(b, _) => Some(b.encode_utf16().collect()),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FunctionKind {
    Normal,
    Generator,
    Async,
    AsyncGenerator,
}

impl FunctionKind {
    pub fn is_async(self) -> bool {
        matches!(self, FunctionKind::Async | FunctionKind::AsyncGenerator)
    }
    pub fn is_generator(self) -> bool {
        matches!(self, FunctionKind::Generator | FunctionKind::AsyncGenerator)
    }
}

#[derive(Debug)]
pub struct Function {
    /// `None` for anonymous expressions and `export default function () {}`.
    pub id: Option<Ident>,
    pub params: Vec<Pattern>,
    pub body: FunctionBody,
    pub kind: FunctionKind,
    pub strict: bool,
    /// All parameters are plain identifiers (no defaults, rest or
    /// destructuring): the arguments object is mapped in sloppy code.
    pub simple_params: bool,
    /// The function body or parameters contain a direct `eval` call, so
    /// the compiler cannot resolve scopes statically.
    pub has_direct_eval: bool,
    pub is_expression: bool,
    pub span: Span,
}

#[derive(Debug)]
pub struct FunctionBody {
    pub body: Vec<Statement>,
    pub span: Span,
}

#[derive(Debug)]
pub enum ArrowBody {
    Block(FunctionBody),
    Expression(Expression),
}

#[derive(Debug)]
pub struct ArrowFunction {
    pub params: Vec<Pattern>,
    pub body: ArrowBody,
    pub is_async: bool,
    pub strict: bool,
    pub simple_params: bool,
    pub has_direct_eval: bool,
    pub span: Span,
}

#[derive(Debug)]
pub struct Class {
    pub id: Option<Ident>,
    pub super_class: Option<Expression>,
    pub body: Vec<ClassMember>,
    pub span: Span,
}

#[derive(Debug)]
pub enum ClassMember {
    Method(ClassMethod),
    Field(ClassField),
    StaticBlock(StaticBlock),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MethodKind {
    Constructor,
    Method,
    Get,
    Set,
}

#[derive(Debug)]
pub struct ClassMethod {
    pub key: PropertyKey,
    pub value: Function,
    pub kind: MethodKind,
    pub is_static: bool,
    pub span: Span,
}

#[derive(Debug)]
pub struct ClassField {
    pub key: PropertyKey,
    pub value: Option<Expression>,
    pub is_static: bool,
    pub span: Span,
}

#[derive(Debug)]
pub struct StaticBlock {
    pub body: Vec<Statement>,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnaryOp {
    Minus,
    Plus,
    Not,
    BitNot,
    Typeof,
    Void,
    Delete,
}

#[derive(Debug)]
pub struct UnaryExpression {
    pub op: UnaryOp,
    pub argument: Expression,
    pub span: Span,
}

#[derive(Debug)]
pub struct UpdateExpression {
    pub increment: bool,
    pub prefix: bool,
    pub argument: Expression,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinaryOp {
    EqEq,
    NotEq,
    EqEqEq,
    NotEqEq,
    Lt,
    LtEq,
    Gt,
    GtEq,
    Shl,
    Shr,
    UShr,
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Exp,
    BitOr,
    BitXor,
    BitAnd,
    In,
    Instanceof,
}

#[derive(Debug)]
pub struct BinaryExpression {
    pub op: BinaryOp,
    pub left: Expression,
    pub right: Expression,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogicalOp {
    Or,
    And,
    Nullish,
}

#[derive(Debug)]
pub struct LogicalExpression {
    pub op: LogicalOp,
    pub left: Expression,
    pub right: Expression,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssignOp {
    Assign,
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Exp,
    Shl,
    Shr,
    UShr,
    BitOr,
    BitXor,
    BitAnd,
    Or,
    And,
    Nullish,
}

#[derive(Debug)]
pub struct AssignmentExpression {
    pub op: AssignOp,
    pub left: Pattern,
    pub right: Expression,
    pub span: Span,
}

#[derive(Debug)]
pub struct ConditionalExpression {
    pub test: Expression,
    pub consequent: Expression,
    pub alternate: Expression,
    pub span: Span,
}

#[derive(Debug)]
pub struct CallExpression {
    pub callee: Expression,
    pub arguments: Vec<Expression>,
    /// `a?.()`: short-circuits when the callee is nullish.
    pub optional: bool,
    pub span: Span,
}

#[derive(Debug)]
pub struct NewExpression {
    pub callee: Expression,
    pub arguments: Vec<Expression>,
    pub span: Span,
}

#[derive(Debug)]
pub enum MemberProperty {
    Ident(Ident),
    Private(PrivateName),
    Computed(Expression),
}

#[derive(Debug)]
pub struct MemberExpression {
    pub object: Expression,
    pub property: MemberProperty,
    /// `a?.b`: short-circuits when the object is nullish.
    pub optional: bool,
    pub span: Span,
}

#[derive(Debug)]
pub struct SequenceExpression {
    pub expressions: Vec<Expression>,
    pub span: Span,
}

#[derive(Debug)]
pub struct YieldExpression {
    pub argument: Option<Expression>,
    pub delegate: bool,
    pub span: Span,
}

#[derive(Debug)]
pub struct AwaitExpression {
    pub argument: Expression,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MetaPropertyKind {
    NewTarget,
    ImportMeta,
}

#[derive(Debug)]
pub struct MetaProperty {
    pub kind: MetaPropertyKind,
    pub span: Span,
}

#[derive(Debug)]
pub struct ImportCall {
    pub source: Expression,
    pub options: Option<Expression>,
    pub span: Span,
}

#[derive(Debug)]
pub struct SpreadElement {
    pub argument: Expression,
    pub span: Span,
}

#[derive(Debug)]
pub struct PrivateInExpression {
    pub left: PrivateName,
    pub right: Expression,
    pub span: Span,
}

// ---- modules ------------------------------------------------------------

/// A module export or import name: an identifier or a string literal
/// (arbitrary module namespace names).
#[derive(Debug, Clone)]
pub enum ModuleExportName {
    Ident(Ident),
    String(StringLiteral),
}

impl ModuleExportName {
    pub fn span(&self) -> Span {
        match self {
            ModuleExportName::Ident(i) => i.span,
            ModuleExportName::String(s) => s.span,
        }
    }
    pub fn as_utf16(&self) -> Vec<u16> {
        match self {
            ModuleExportName::Ident(i) => i.name.encode_utf16().collect(),
            ModuleExportName::String(s) => s.value.to_vec(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ImportAttribute {
    pub key: ModuleExportName,
    pub value: StringLiteral,
    pub span: Span,
}

#[derive(Debug)]
pub struct ImportDeclaration {
    pub specifiers: Vec<ImportSpecifier>,
    pub source: StringLiteral,
    pub attributes: Vec<ImportAttribute>,
    pub span: Span,
}

#[derive(Debug)]
pub enum ImportSpecifier {
    /// `import x from`
    Default(Ident),
    /// `import * as ns from`
    Namespace(Ident),
    /// `import { a as b } from`
    Named {
        imported: ModuleExportName,
        local: Ident,
        span: Span,
    },
}

#[derive(Debug)]
pub struct ExportNamedDeclaration {
    /// `export var/let/const/function/class ...`
    pub declaration: Option<Statement>,
    /// `export { a as b }` (possibly `from`).
    pub specifiers: Vec<ExportSpecifier>,
    pub source: Option<StringLiteral>,
    pub attributes: Vec<ImportAttribute>,
    pub span: Span,
}

#[derive(Debug)]
pub struct ExportSpecifier {
    pub local: ModuleExportName,
    pub exported: ModuleExportName,
    pub span: Span,
}

#[derive(Debug)]
pub enum ExportDefaultKind {
    Function(Box<Function>),
    Class(Box<Class>),
    Expression(Expression),
}

#[derive(Debug)]
pub struct ExportDefaultDeclaration {
    pub declaration: ExportDefaultKind,
    pub span: Span,
}

#[derive(Debug)]
pub struct ExportAllDeclaration {
    /// `export * as name from`
    pub exported: Option<ModuleExportName>,
    pub source: StringLiteral,
    pub attributes: Vec<ImportAttribute>,
    pub span: Span,
}
