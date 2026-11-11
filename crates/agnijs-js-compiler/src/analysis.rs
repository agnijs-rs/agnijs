//! Pre-pass over a function (or program) body that decides what the
//! code generator needs to know up front:
//!
//! - which names are referenced from nested functions (those bindings
//!   must live in heap environments rather than registers),
//! - whether a direct `eval` appears anywhere inside (then every binding
//!   in scope must be reachable by name at runtime),
//! - which names are referenced inside `with` bodies,
//! - whether the function itself (or an arrow inside it) uses `this`,
//!   `new.target`, `super`, or `arguments`.
//!
//! The analysis is name based and conservative: a name referenced by any
//! nested function marks every enclosing binding of that name as captured,
//! even when an intermediate scope shadows it.

use agnijs_js_parser::ast::*;
use foldhash::{HashMap, HashSet};
use std::rc::Rc;

#[derive(Default, Debug, Clone)]
pub struct Uses {
    pub this: bool,
    pub new_target: bool,
    /// `super.x` or `super()`: needs the active function (home object).
    pub super_: bool,
    pub arguments: bool,
}

impl Uses {
    fn merge(&mut self, o: &Uses) {
        self.this |= o.this;
        self.new_target |= o.new_target;
        self.super_ |= o.super_;
        self.arguments |= o.arguments;
    }
    pub fn any(&self) -> bool {
        self.this || self.new_target || self.super_ || self.arguments
    }
}

#[derive(Default, Debug)]
pub struct FnAnalysis {
    /// Names referenced anywhere in this function, including nested ones.
    pub all_names: HashSet<Rc<str>>,
    /// Names referenced from nested functions (any depth).
    pub captured_names: HashSet<Rc<str>>,
    /// Names referenced inside a `with` body (any depth).
    pub with_names: HashSet<Rc<str>>,
    /// A direct `eval(...)` call appears in this function or any nested
    /// function.
    pub has_eval_inside: bool,
    /// A direct `eval(...)` call appears in this function itself (not
    /// counting nested non-arrow functions).
    pub own_eval: bool,
    /// `this` / `new.target` / `super` / `arguments` used by this function
    /// itself or by arrows nested in it.
    pub uses: Uses,
    /// Pseudo-bindings used by nested arrows (must be heap allocated).
    pub captured_uses: Uses,
    /// Top-level `await` (module code).
    pub has_tla: bool,
    /// Contains a `with` statement.
    pub contains_with: bool,
}

/// Analysis results for every function in a program, keyed by the
/// function node's address.
#[derive(Default)]
pub struct Analyses {
    map: HashMap<usize, Rc<FnAnalysis>>,
    /// The walk ran out of an installed stack budget
    /// (`agnijs_js_parser::stack`) and is incomplete.
    pub too_deep: bool,
}

fn key<T>(node: &T) -> usize {
    node as *const T as usize
}

impl Analyses {
    pub fn get<T>(&self, node: &T) -> Rc<FnAnalysis> {
        self.map
            .get(&key(node))
            .cloned()
            .unwrap_or_else(|| panic!("node was not analysed"))
    }
}

struct Walker<'a> {
    out: &'a mut Analyses,
    cur: FnAnalysis,
    with_depth: u32,
    /// Depth of nested (non-arrow) functions seen from the current
    /// function: 0 when at the function's own level.
    arrow_depth: u32,
}

/// Analyse a whole program. The program itself is registered under the
/// `Program` node's address.
pub fn analyse_program(program: &Program) -> Analyses {
    let mut out = Analyses::default();
    let mut w = Walker {
        out: &mut out,
        cur: FnAnalysis::default(),
        with_depth: 0,
        arrow_depth: 0,
    };
    for s in &program.body {
        w.stmt(s);
    }
    let a = std::mem::take(&mut w.cur);
    out.map.insert(key(program), Rc::new(a));
    out
}

impl<'a> Walker<'a> {
    fn name(&mut self, n: &Rc<str>) {
        self.cur.all_names.insert(n.clone());
        if self.with_depth > 0 {
            self.cur.with_names.insert(n.clone());
        }
        if &**n == "arguments" {
            self.cur.uses.arguments = true;
            if self.arrow_depth > 0 {
                self.cur.captured_uses.arguments = true;
            }
        }
    }

    /// Walk a nested function body in a fresh analysis and merge the
    /// result into the current one.
    fn nested<F: FnOnce(&mut Walker)>(&mut self, node_key: usize, is_arrow: bool, f: F) {
        let saved = std::mem::take(&mut self.cur);
        let saved_with = self.with_depth;
        let saved_arrow = self.arrow_depth;
        self.with_depth = 0;
        self.arrow_depth = 0;
        f(self);
        let child = std::mem::replace(&mut self.cur, saved);
        self.with_depth = saved_with;
        self.arrow_depth = saved_arrow;
        // Merge.
        for n in &child.all_names {
            self.cur.all_names.insert(n.clone());
            self.cur.captured_names.insert(n.clone());
            if self.with_depth > 0 {
                self.cur.with_names.insert(n.clone());
            }
        }
        for n in &child.with_names {
            self.cur.with_names.insert(n.clone());
        }
        self.cur.has_eval_inside |= child.has_eval_inside;
        if is_arrow {
            self.cur.uses.merge(&child.uses);
            self.cur.captured_uses.merge(&child.uses);
            self.cur.captured_uses.merge(&child.captured_uses);
            if child.own_eval {
                // eval inside an arrow sees the enclosing this etc.
                let all = Uses {
                    this: true,
                    new_target: true,
                    super_: true,
                    arguments: true,
                };
                self.cur.uses.merge(&all);
                self.cur.captured_uses.merge(&all);
            }
        }
        self.out.map.insert(node_key, Rc::new(child));
    }

    fn function(&mut self, f: &Function) {
        let k = key(f);
        self.nested(k, false, |w| {
            for p in &f.params {
                w.pattern(p);
            }
            for s in &f.body.body {
                w.stmt(s);
            }
        });
    }

    fn arrow(&mut self, f: &ArrowFunction) {
        let k = key(f);
        self.arrow_depth += 1;
        self.nested(k, true, |w| {
            w.arrow_depth = 1;
            for p in &f.params {
                w.pattern(p);
            }
            match &f.body {
                ArrowBody::Block(b) => {
                    for s in &b.body {
                        w.stmt(s);
                    }
                }
                ArrowBody::Expression(e) => w.expr(e),
            }
        });
        self.arrow_depth -= 1;
    }

    fn class(&mut self, c: &Class) {
        if let Some(h) = &c.super_class {
            self.expr(h);
        }
        for m in &c.body {
            match m {
                ClassMember::Method(m) => {
                    self.property_key(&m.key);
                    self.function(&m.value);
                }
                ClassMember::Field(f) => {
                    self.property_key(&f.key);
                    let k = key(f);
                    self.nested(k, false, |w| {
                        if let Some(v) = &f.value {
                            w.expr(v);
                        }
                    });
                }
                ClassMember::StaticBlock(b) => {
                    let k = key(b);
                    self.nested(k, false, |w| {
                        for s in &b.body {
                            w.stmt(s);
                        }
                    });
                }
            }
        }
    }

    fn property_key(&mut self, k: &PropertyKey) {
        if let PropertyKey::Computed(e) = k {
            self.expr(e);
        }
    }

    fn pattern(&mut self, p: &Pattern) {
        if self.too_deep() {
            return;
        }
        match p {
            Pattern::Ident(i) => self.name(&i.name),
            Pattern::Object(o) => {
                for prop in &o.properties {
                    self.property_key(&prop.key);
                    self.pattern(&prop.value);
                }
                if let Some(r) = &o.rest {
                    self.pattern(r);
                }
            }
            Pattern::Array(a) => {
                for e in a.elements.iter().flatten() {
                    self.pattern(e);
                }
            }
            Pattern::Expression(e) => self.expr(e),
            Pattern::Assignment(a) => {
                self.pattern(&a.left);
                self.expr(&a.right);
            }
            Pattern::Rest(r) => self.pattern(&r.argument),
        }
    }

    fn var_decl(&mut self, d: &VariableDeclaration) {
        if d.kind == VariableKind::AwaitUsing && self.arrow_depth == 0 {
            self.cur.has_tla = true;
        }
        for decl in &d.declarations {
            self.pattern(&decl.id);
            if let Some(i) = &decl.init {
                self.expr(i);
            }
        }
    }

    fn stmt(&mut self, s: &Statement) {
        if self.too_deep() {
            return;
        }
        match s {
            Statement::Block(b) => self.stmts(&b.body),
            Statement::Empty(_) | Statement::Debugger(_) => {}
            Statement::Expression(e) => self.expr(&e.expression),
            Statement::If(i) => {
                self.expr(&i.test);
                self.stmt(&i.consequent);
                if let Some(a) = &i.alternate {
                    self.stmt(a);
                }
            }
            Statement::Labeled(l) => self.stmt(&l.body),
            Statement::Break(_) | Statement::Continue(_) => {}
            Statement::With(w) => {
                self.expr(&w.object);
                self.cur.contains_with = true;
                self.with_depth += 1;
                self.stmt(&w.body);
                self.with_depth -= 1;
            }
            Statement::Switch(sw) => {
                self.expr(&sw.discriminant);
                for c in &sw.cases {
                    if let Some(t) = &c.test {
                        self.expr(t);
                    }
                    self.stmts(&c.consequent);
                }
            }
            Statement::Return(r) => {
                if let Some(a) = &r.argument {
                    self.expr(a);
                }
            }
            Statement::Throw(t) => self.expr(&t.argument),
            Statement::Try(t) => {
                self.stmts(&t.block.body);
                if let Some(h) = &t.handler {
                    if let Some(p) = &h.param {
                        self.pattern(p);
                    }
                    self.stmts(&h.body.body);
                }
                if let Some(f) = &t.finalizer {
                    self.stmts(&f.body);
                }
            }
            Statement::While(w) => {
                self.expr(&w.test);
                self.stmt(&w.body);
            }
            Statement::DoWhile(d) => {
                self.stmt(&d.body);
                self.expr(&d.test);
            }
            Statement::For(f) => {
                match &f.init {
                    Some(ForInit::VariableDeclaration(d)) => self.var_decl(d),
                    Some(ForInit::Expression(e)) => self.expr(e),
                    None => {}
                }
                if let Some(t) = &f.test {
                    self.expr(t);
                }
                if let Some(u) = &f.update {
                    self.expr(u);
                }
                self.stmt(&f.body);
            }
            Statement::ForIn(f) => {
                self.for_left(&f.left);
                self.expr(&f.right);
                self.stmt(&f.body);
            }
            Statement::ForOf(f) => {
                if f.is_await && self.arrow_depth == 0 {
                    // Only module top level can have for-await outside an
                    // async function; the parser enforces it.
                    self.cur.has_tla = true;
                }
                self.for_left(&f.left);
                self.expr(&f.right);
                self.stmt(&f.body);
            }
            Statement::VariableDeclaration(d) => self.var_decl(d),
            Statement::FunctionDeclaration(f) => {
                if let Some(id) = &f.id {
                    self.name(&id.name);
                }
                self.function(f);
            }
            Statement::ClassDeclaration(c) => {
                if let Some(id) = &c.id {
                    self.name(&id.name);
                }
                self.class(c);
            }
            Statement::Import(i) => {
                for s in &i.specifiers {
                    match s {
                        ImportSpecifier::Default(l) | ImportSpecifier::Namespace(l) => {
                            self.name(&l.name)
                        }
                        ImportSpecifier::Named { local, .. } => self.name(&local.name),
                    }
                }
            }
            Statement::ExportNamed(e) => {
                if let Some(d) = &e.declaration {
                    self.stmt(d);
                }
                if e.source.is_none() {
                    for s in &e.specifiers {
                        if let ModuleExportName::Ident(i) = &s.local {
                            self.name(&i.name);
                        }
                    }
                }
            }
            Statement::ExportDefault(e) => match &e.declaration {
                ExportDefaultKind::Function(f) => {
                    if let Some(id) = &f.id {
                        self.name(&id.name);
                    }
                    self.function(f);
                }
                ExportDefaultKind::Class(c) => {
                    if let Some(id) = &c.id {
                        self.name(&id.name);
                    }
                    self.class(c);
                }
                ExportDefaultKind::Expression(x) => self.expr(x),
            },
            Statement::ExportAll(_) => {}
        }
    }

    fn for_left(&mut self, l: &ForLeft) {
        match l {
            ForLeft::VariableDeclaration(d) => self.var_decl(d),
            ForLeft::Pattern(p) => self.pattern(p),
        }
    }

    fn stmts(&mut self, s: &[Statement]) {
        for st in s {
            self.stmt(st);
        }
    }

    fn template(&mut self, t: &TemplateLiteral) {
        for e in &t.expressions {
            self.expr(e);
        }
    }

    /// Whether an installed stack budget is used up; the walk then stops
    /// descending and the program fails to compile.
    fn too_deep(&mut self) -> bool {
        if agnijs_js_parser::stack::exhausted() {
            self.out.too_deep = true;
        }
        self.out.too_deep
    }

    fn expr(&mut self, e: &Expression) {
        if self.too_deep() {
            return;
        }
        match e {
            Expression::Ident(i) => self.name(&i.name),
            Expression::This(_) => {
                self.cur.uses.this = true;
                if self.arrow_depth > 0 {
                    self.cur.captured_uses.this = true;
                }
            }
            Expression::Super(_) => {
                // `super(...)` needs new.target and the active function as
                // well as `this`; mark them all so an arrow captures them.
                self.cur.uses.super_ = true;
                self.cur.uses.this = true;
                self.cur.uses.new_target = true;
                if self.arrow_depth > 0 {
                    self.cur.captured_uses.super_ = true;
                    self.cur.captured_uses.this = true;
                    self.cur.captured_uses.new_target = true;
                }
            }
            Expression::Null(_)
            | Expression::Bool(..)
            | Expression::Number(..)
            | Expression::BigInt(..)
            | Expression::String(_)
            | Expression::RegExp(_) => {}
            Expression::Template(t) => self.template(t),
            Expression::TaggedTemplate(t) => {
                self.expr(&t.tag);
                self.template(&t.quasi);
            }
            Expression::Array(a) => {
                for el in a.elements.iter().flatten() {
                    self.expr(el);
                }
            }
            Expression::Object(o) => {
                for p in &o.properties {
                    match p {
                        ObjectProperty::Property(p) => {
                            self.property_key(&p.key);
                            if p.shorthand {
                                if let Expression::Ident(i) = &p.value {
                                    self.name(&i.name);
                                    continue;
                                }
                            }
                            self.expr(&p.value);
                        }
                        ObjectProperty::Spread(s) => self.expr(&s.argument),
                    }
                }
            }
            Expression::Function(f) => {
                self.function(f);
            }
            Expression::Arrow(a) => self.arrow(a),
            Expression::Class(c) => self.class(c),
            Expression::Unary(u) => self.expr(&u.argument),
            Expression::Update(u) => self.expr(&u.argument),
            Expression::Binary(b) => {
                self.expr(&b.left);
                self.expr(&b.right);
            }
            Expression::Logical(l) => {
                self.expr(&l.left);
                self.expr(&l.right);
            }
            Expression::Assignment(a) => {
                self.pattern(&a.left);
                self.expr(&a.right);
            }
            Expression::Conditional(c) => {
                self.expr(&c.test);
                self.expr(&c.consequent);
                self.expr(&c.alternate);
            }
            Expression::Call(c) => {
                if let Expression::Ident(i) = &c.callee {
                    if &*i.name == "eval" {
                        self.cur.has_eval_inside = true;
                        self.cur.own_eval = true;
                        // eval code may use any of these.
                        let all = Uses {
                            this: true,
                            new_target: true,
                            super_: true,
                            arguments: true,
                        };
                        self.cur.uses.merge(&all);
                        if self.arrow_depth > 0 {
                            self.cur.captured_uses.merge(&all);
                        }
                    }
                }
                self.expr(&c.callee);
                for a in &c.arguments {
                    self.expr(a);
                }
            }
            Expression::New(n) => {
                self.expr(&n.callee);
                for a in &n.arguments {
                    self.expr(a);
                }
            }
            Expression::Member(m) => {
                self.expr(&m.object);
                match &m.property {
                    MemberProperty::Computed(c) => self.expr(c),
                    MemberProperty::Private(p) => self.name(&private_binding_name(&p.name)),
                    MemberProperty::Ident(_) => {}
                }
            }
            Expression::OptionalChain(inner, _) => self.expr(inner),
            Expression::Sequence(s) => {
                for x in &s.expressions {
                    self.expr(x);
                }
            }
            Expression::Yield(y) => {
                if let Some(a) = &y.argument {
                    self.expr(a);
                }
            }
            Expression::Await(a) => {
                if self.arrow_depth == 0 {
                    self.cur.has_tla = true;
                }
                self.expr(&a.argument);
            }
            Expression::MetaProperty(m) => {
                if m.kind == MetaPropertyKind::NewTarget {
                    self.cur.uses.new_target = true;
                    if self.arrow_depth > 0 {
                        self.cur.captured_uses.new_target = true;
                    }
                }
            }
            Expression::Import(i) => {
                self.expr(&i.source);
                if let Some(o) = &i.options {
                    self.expr(o);
                }
            }
            Expression::Spread(s) => self.expr(&s.argument),
            Expression::PrivateIn(p) => {
                self.name(&private_binding_name(&p.left.name));
                self.expr(&p.right);
            }
            Expression::Paren(inner, _) => self.expr(inner),
        }
    }
}

/// Private names are tracked as pseudo-bindings named `#x`.
pub fn private_binding_name(name: &str) -> Rc<str> {
    Rc::from(format!("#{name}"))
}
