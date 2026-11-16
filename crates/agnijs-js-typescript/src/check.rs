//! The constructs the crate refuses before Oxc transforms anything: the
//! ones that would not run on the engine, and in [`Mode::StripOnly`] the
//! ones that are not erasable. Bodies of ambient declarations (`declare`,
//! `declare module "x"`, `declare global`) hold no code and are skipped.

use std::collections::{HashMap, HashSet};

use oxc_allocator::ArenaVec;
use oxc_ast::ast::{
    AccessorProperty, ArrowFunctionExpression, AwaitExpression, BindingIdentifier, Class,
    ClassElement, Declaration, Decorator, ForOfStatement, FormalParameter, Function,
    IdentifierName, IdentifierReference, LabelIdentifier, PrivateIdentifier, ImportOrExportKind, Program, Statement, TSEnumDeclaration,
    TSExportAssignment, TSExternalModuleDeclaration, TSGlobalDeclaration,
    TSImportEqualsDeclaration, TSModuleBlock, TSModuleReference, TSNamespaceDeclaration,
    TSNamespaceDeclarationBody, VariableDeclaration, VariableDeclarationKind, YieldExpression,
};
use oxc_ast_visit::{walk, Visit};
use oxc_span::{GetSpan, Span};
use oxc_syntax::scope::ScopeFlags;

use crate::{Mode, Options};

/// A refusal: message, help, span.
pub(crate) struct Refusal {
    pub message: String,
    pub help: Option<String>,
    pub span: Span,
}

pub(crate) struct Checked {
    pub refusals: Vec<Refusal>,
    /// A decorator was seen (and is allowed): the output calls the helpers.
    pub uses_decorators: bool,
    /// The indentation the printer would emit, in levels: the sum of the
    /// nesting depths of the statements and class members. It grows with
    /// the square of the nesting, so hostile input can make it far larger
    /// than the source (see `pipeline::run`).
    pub indent_levels: u64,
}

pub(crate) fn check(program: &Program<'_>, options: &Options) -> Checked {
    let mut c = Checker {
        options,
        source: program.source_text,
        refusals: Vec::new(),
        uses_decorators: false,
        depth: 0,
        indent_levels: 0,
        merged_blocks: HashMap::new(),
        merged: Vec::new(),
        module_items: true,
        in_class_expression: false,
        wrapped: None,
        wrapped_arguments: None,
    };
    c.visit_program(program);
    c.refusals.sort_by_key(|r| (r.span.start, r.span.end));
    Checked {
        refusals: c.refusals,
        uses_decorators: c.uses_decorators,
        indent_levels: c.indent_levels,
    }
}

struct Checker<'o> {
    options: &'o Options,
    source: &'o str,
    refusals: Vec<Refusal>,
    uses_decorators: bool,
    depth: u64,
    indent_levels: u64,
    /// For each block of a namespace declared in several blocks, by the
    /// start of its declaration: the namespace's name and the names other
    /// blocks export and this one does not declare.
    merged_blocks: HashMap<u32, (String, HashSet<String>)>,
    /// The entries of `merged_blocks` for the blocks being walked.
    merged: Vec<(String, HashSet<String>)>,
    /// Whether the statements being walked may be imports and exports:
    /// the program's and a namespace body's. Oxc's TypeScript parser also
    /// accepts them in blocks, functions and labels, where the engine
    /// refuses them.
    module_items: bool,
    /// Whether the innermost class being walked is a class expression:
    /// Oxc's legacy decorator transform leaves the decorators of one in
    /// place and drops those of its members.
    in_class_expression: bool,
    /// Inside an enum member's initializer or a namespace body, which the
    /// transform moves into a function: what it is, for the message, until
    /// a nested function or arrow, where `await` and `yield` are its own.
    wrapped: Option<&'static str>,
    /// The same, until a nested function, where `arguments` is its own
    /// (an arrow's `arguments` is the wrapper's).
    wrapped_arguments: Option<&'static str>,
}

impl Checker<'_> {
    fn strip_only(&self) -> bool {
        self.options.mode == Mode::StripOnly
    }

    fn refuse(&mut self, span: Span, message: impl Into<String>, help: Option<&str>) {
        self.refusals.push(Refusal {
            message: message.into(),
            help: help.map(str::to_string),
            span,
        });
    }

    /// Oxc's TypeScript parser loses lexer errors when it looks ahead for
    /// type arguments and rewinds (any `<` after the error), and then
    /// prints an identifier with a bad escape without it: `a\x41` as
    /// `a41`. Every escaped identifier is checked here instead.
    fn check_escapes(&mut self, span: Span, name: &str) {
        let raw = self.source.get(span.start as usize..span.end as usize).unwrap_or("");
        let raw = raw.strip_prefix('#').unwrap_or(raw);
        if raw.contains('\\') && !escapes_spell(raw, name) {
            self.refuse(
                span,
                "Invalid Unicode escape sequence",
                Some("an identifier may only escape characters it could hold, as \\uXXXX or \\u{X}"),
            );
        }
    }

    /// Code the transform moves into a function, where it would not run or
    /// would mean something else.
    fn refuse_wrapped(&mut self, span: Span, what: &str, wrapped: &str) {
        self.refuse(
            span,
            format!("{what} in {wrapped}, which becomes a function"),
            Some("compute the value outside it"),
        );
    }

    /// Walk with `wrapped` set (an enum or a namespace body).
    fn wrapping(&mut self, wrapped: &'static str, walk: impl FnOnce(&mut Self)) {
        let outer = (self.wrapped, self.wrapped_arguments);
        self.wrapped = Some(wrapped);
        self.wrapped_arguments = Some(wrapped);
        walk(self);
        (self.wrapped, self.wrapped_arguments) = outer;
    }

    fn refuse_not_erasable(&mut self, span: Span, what: &str) {
        self.refuse(
            span,
            format!("{what} is not erasable syntax, and the mode is strip-only"),
            Some("use Mode::Transform, or rewrite it as plain JavaScript"),
        );
    }
}

impl<'a> Visit<'a> for Checker<'_> {
    fn visit_statement(&mut self, it: &Statement<'a>) {
        if !self.module_items && it.is_module_declaration() {
            self.refuse(
                it.span(),
                "import and export declarations may only appear at the top level of a module or a namespace",
                None,
            );
        }
        let module_items = std::mem::replace(&mut self.module_items, false);
        self.indent_levels = self.indent_levels.saturating_add(self.depth);
        self.depth += 1;
        walk::walk_statement(self, it);
        self.depth -= 1;
        self.module_items = module_items;
    }

    fn visit_ts_module_block(&mut self, it: &TSModuleBlock<'a>) {
        let module_items = std::mem::replace(&mut self.module_items, true);
        walk::walk_ts_module_block(self, it);
        self.module_items = module_items;
    }

    fn visit_class_element(&mut self, it: &ClassElement<'a>) {
        self.indent_levels = self.indent_levels.saturating_add(self.depth);
        self.depth += 1;
        walk::walk_class_element(self, it);
        self.depth -= 1;
    }

    fn visit_decorator(&mut self, it: &Decorator<'a>) {
        if self.strip_only() {
            self.refuse_not_erasable(it.span, "a decorator");
        } else if !self.options.experimental_decorators {
            self.refuse(
                it.span,
                "decorators need Options::experimental_decorators",
                Some(
                    "only TypeScript's legacy decorators (experimentalDecorators) are transformed; \
                     standard decorators are not supported",
                ),
            );
        } else if self.in_class_expression {
            self.refuse(
                it.span,
                "legacy decorators apply to class declarations, not class expressions",
                Some("declare the class, then use its name"),
            );
        } else {
            self.uses_decorators = true;
        }
        walk::walk_decorator(self, it);
    }

    fn visit_accessor_property(&mut self, it: &AccessorProperty<'a>) {
        self.refuse(
            it.span,
            "`accessor` fields are not supported",
            Some("use a getter and a setter"),
        );
        walk::walk_accessor_property(self, it);
    }

    fn visit_ts_import_equals_declaration(&mut self, it: &TSImportEqualsDeclaration<'a>) {
        if let TSModuleReference::ExternalModuleReference(_) = it.module_reference {
            if it.import_kind == ImportOrExportKind::Value {
                self.refuse(
                    it.span,
                    "`import x = require()` is CommonJS, which the engine does not provide",
                    Some("use `import * as x from \"...\"`"),
                );
            }
        } else if self.strip_only() && it.import_kind == ImportOrExportKind::Value {
            self.refuse_not_erasable(it.span, "an `import x = y` alias");
        }
        walk::walk_ts_import_equals_declaration(self, it);
    }

    fn visit_ts_export_assignment(&mut self, it: &TSExportAssignment<'a>) {
        self.refuse(
            it.span,
            "`export =` is CommonJS, which the engine does not provide",
            Some("use `export default`"),
        );
        walk::walk_ts_export_assignment(self, it);
    }

    fn visit_ts_enum_declaration(&mut self, it: &TSEnumDeclaration<'a>) {
        if it.declare {
            return;
        }
        if self.strip_only() {
            self.refuse_not_erasable(
                it.span,
                if it.r#const {
                    "a `const enum`"
                } else {
                    "an `enum`"
                },
            );
        }
        self.wrapping("an enum", |c| walk::walk_ts_enum_declaration(c, it));
    }

    fn visit_ts_namespace_declaration(&mut self, it: &TSNamespaceDeclaration<'a>) {
        if it.declare {
            return;
        }
        if self.strip_only() && namespace_is_instantiated(it) {
            self.refuse_not_erasable(it.span, "a namespace with values");
        }
        let merged = self.merged_blocks.remove(&it.span.start);
        let pushed = merged.is_some();
        self.merged.extend(merged);
        self.wrapping("a namespace", |c| walk::walk_ts_namespace_declaration(c, it));
        if pushed {
            self.merged.pop();
        }
    }

    fn visit_statements(&mut self, it: &ArenaVec<'a, Statement<'a>>) {
        find_merged_namespaces(it, &mut self.merged_blocks);
        walk::walk_statements(self, it);
    }

    fn visit_identifier_name(&mut self, it: &IdentifierName<'a>) {
        self.check_escapes(it.span, &it.name);
    }

    fn visit_binding_identifier(&mut self, it: &BindingIdentifier<'a>) {
        self.check_escapes(it.span, &it.name);
    }

    fn visit_label_identifier(&mut self, it: &LabelIdentifier<'a>) {
        self.check_escapes(it.span, &it.name);
    }

    fn visit_private_identifier(&mut self, it: &PrivateIdentifier<'a>) {
        self.check_escapes(it.span, &it.name);
    }

    /// Oxc's TypeScript parser skips "Expected function name" when type
    /// parameters follow `function` (`function<T>() {}`), and prints a
    /// declaration the engine cannot parse. `export default function` is
    /// not a `Declaration`, so it does not come through here.
    fn visit_declaration(&mut self, it: &Declaration<'a>) {
        let nameless = match it {
            Declaration::FunctionDeclaration(f) if f.id.is_none() => Some("Expected function name"),
            Declaration::ClassDeclaration(c) if c.id.is_none() => Some("A class name is required."),
            _ => None,
        };
        if let Some(message) = nameless {
            self.refuse(
                it.span(),
                message,
                Some("a declaration needs a name, except after `export default`"),
            );
        }
        walk::walk_declaration(self, it);
    }

    fn visit_function(&mut self, it: &Function<'a>, flags: ScopeFlags) {
        let outer = (self.wrapped.take(), self.wrapped_arguments.take());
        walk::walk_function(self, it, flags);
        (self.wrapped, self.wrapped_arguments) = outer;
    }

    fn visit_arrow_function_expression(&mut self, it: &ArrowFunctionExpression<'a>) {
        let outer = self.wrapped.take();
        walk::walk_arrow_function_expression(self, it);
        self.wrapped = outer;
    }

    fn visit_await_expression(&mut self, it: &AwaitExpression<'a>) {
        if let Some(w) = self.wrapped {
            self.refuse_wrapped(it.span, "`await`", w);
        }
        walk::walk_await_expression(self, it);
    }

    fn visit_yield_expression(&mut self, it: &YieldExpression<'a>) {
        if let Some(w) = self.wrapped {
            self.refuse_wrapped(it.span, "`yield`", w);
        }
        walk::walk_yield_expression(self, it);
    }

    fn visit_for_of_statement(&mut self, it: &ForOfStatement<'a>) {
        if let (true, Some(w)) = (it.r#await, self.wrapped) {
            self.refuse_wrapped(it.span, "`for await`", w);
        }
        walk::walk_for_of_statement(self, it);
    }

    fn visit_variable_declaration(&mut self, it: &VariableDeclaration<'a>) {
        if let (VariableDeclarationKind::AwaitUsing, Some(w)) = (it.kind, self.wrapped) {
            self.refuse_wrapped(it.span, "`await using`", w);
        }
        walk::walk_variable_declaration(self, it);
    }

    fn visit_identifier_reference(&mut self, it: &IdentifierReference<'a>) {
        self.check_escapes(it.span, &it.name);
        if let (true, Some(w)) = (it.name == "arguments", self.wrapped_arguments) {
            self.refuse_wrapped(it.span, "`arguments`", w);
        }
        let name = it.name.as_str();
        if let Some((ns, _)) = self
            .merged
            .iter()
            .rev()
            .find(|(_, names)| names.contains(name))
        {
            let message = format!(
                "`{name}` is exported by another declaration of namespace `{ns}`, which the transform does not resolve"
            );
            let help = format!("refer to it as `{ns}.{name}`");
            self.refuse(it.span, message, Some(&help));
        }
    }

    fn visit_ts_external_module_declaration(&mut self, _it: &TSExternalModuleDeclaration<'a>) {}

    fn visit_ts_global_declaration(&mut self, _it: &TSGlobalDeclaration<'a>) {}

    fn visit_class(&mut self, it: &Class<'a>) {
        if it.declare {
            return;
        }
        let outer = std::mem::replace(&mut self.in_class_expression, it.is_expression());
        walk::walk_class(self, it);
        self.in_class_expression = outer;
    }

    fn visit_formal_parameter(&mut self, it: &FormalParameter<'a>) {
        if self.strip_only() && (it.accessibility.is_some() || it.readonly || it.r#override) {
            self.refuse_not_erasable(it.span, "a parameter property");
        }
        walk::walk_formal_parameter(self, it);
    }
}

/// Whether a namespace emits code: TypeScript's rule, that a namespace
/// holding only types, ambient declarations, type-only aliases and such
/// namespaces is erased.
fn namespace_is_instantiated(ns: &TSNamespaceDeclaration<'_>) -> bool {
    match &ns.body {
        TSNamespaceDeclarationBody::TSNamespaceDeclaration(inner) => {
            !inner.declare && namespace_is_instantiated(inner)
        }
        TSNamespaceDeclarationBody::TSModuleBlock(block) => {
            block.body.iter().any(statement_is_instantiated)
        }
    }
}

fn statement_is_instantiated(stmt: &Statement<'_>) -> bool {
    match stmt {
        Statement::EmptyStatement(_) => false,
        Statement::ExportDeclaration(export) => declaration_is_instantiated(&export.declaration),
        Statement::ExportNamedDeclaration(export) => {
            export.export_kind == ImportOrExportKind::Value
        }
        _ => stmt
            .as_declaration()
            .is_none_or(declaration_is_instantiated),
    }
}

fn declaration_is_instantiated(decl: &Declaration<'_>) -> bool {
    if decl.declare() || decl.is_type() {
        return false;
    }
    match decl {
        Declaration::TSNamespaceDeclaration(ns) => namespace_is_instantiated(ns),
        Declaration::TSImportEqualsDeclaration(alias) => {
            alias.import_kind == ImportOrExportKind::Value
        }
        Declaration::TSGlobalDeclaration(_) | Declaration::TSExternalModuleDeclaration(_) => false,
        _ => true,
    }
}

/// TypeScript merges the blocks of a namespace declared more than once, so
/// a block can name what another block exports without qualifying it, and
/// tsc rewrites the reference. Oxc's transform does not: the reference
/// would be left unbound and fail at run time. Records, for each block of
/// such a namespace in `stmts`, the names it could refer to that way.
fn find_merged_namespaces(
    stmts: &[Statement<'_>],
    out: &mut HashMap<u32, (String, HashSet<String>)>,
) {
    let mut groups: HashMap<&str, Vec<(u32, &TSModuleBlock<'_>)>> = HashMap::new();
    for stmt in stmts {
        let decl = match stmt {
            Statement::ExportDeclaration(e) => &e.declaration,
            _ => match stmt.as_declaration() {
                Some(d) => d,
                None => continue,
            },
        };
        if let Declaration::TSNamespaceDeclaration(ns) = decl {
            if let (false, TSNamespaceDeclarationBody::TSModuleBlock(block)) =
                (ns.declare, &ns.body)
            {
                groups
                    .entry(ns.id.name.as_str())
                    .or_default()
                    .push((ns.span.start, block));
            }
        }
    }
    for (name, blocks) in groups {
        if blocks.len() < 2 {
            continue;
        }
        let names: Vec<(HashSet<String>, HashSet<String>)> =
            blocks.iter().map(|(_, block)| block_names(block)).collect();
        for (i, (start, _)) in blocks.iter().enumerate() {
            let declared = &names[i].1;
            let others: HashSet<String> = names
                .iter()
                .enumerate()
                .filter(|(j, _)| *j != i)
                .flat_map(|(_, (exported, _))| exported.iter())
                .filter(|n| !declared.contains(*n))
                .cloned()
                .collect();
            if !others.is_empty() {
                out.insert(*start, (name.to_string(), others));
            }
        }
    }
}

/// The value names a namespace block exports, and all it declares.
fn block_names(block: &TSModuleBlock<'_>) -> (HashSet<String>, HashSet<String>) {
    let mut exported = HashSet::new();
    let mut declared = HashSet::new();
    for stmt in &block.body {
        let (decl, is_export) = match stmt {
            Statement::ExportDeclaration(e) => (&e.declaration, true),
            _ => match stmt.as_declaration() {
                Some(d) => (d, false),
                None => continue,
            },
        };
        if decl.is_type() {
            continue;
        }
        let mut add = |name: &str| {
            declared.insert(name.to_string());
            if is_export {
                exported.insert(name.to_string());
            }
        };
        match decl {
            Declaration::VariableDeclaration(v) => {
                for d in &v.declarations {
                    for id in d.id.get_binding_identifiers() {
                        add(id.name.as_str());
                    }
                }
            }
            other => {
                if let Some(id) = other.id() {
                    add(id.name.as_str());
                }
            }
        }
    }
    (exported, declared)
}

/// Whether `raw`, an identifier as written, spells `name` with only the
/// escapes ECMAScript allows: `\uXXXX` or `\u{X}`, each of a character the
/// identifier could hold at that place.
fn escapes_spell(raw: &str, name: &str) -> bool {
    use oxc_syntax::identifier::{is_identifier_part, is_identifier_start};
    let mut spelled = String::with_capacity(name.len());
    let mut rest = raw;
    while let Some(i) = rest.find('\\') {
        spelled.push_str(&rest[..i]);
        let Some(after) = rest[i + 1..].strip_prefix('u') else {
            return false;
        };
        let (hex, tail) = match after.strip_prefix('{') {
            Some(braced) => match braced.split_once('}') {
                Some((hex, tail)) if !hex.is_empty() => (hex, tail),
                _ => return false,
            },
            None => match (after.get(..4), after.get(4..)) {
                (Some(hex), Some(tail)) => (hex, tail),
                _ => return false,
            },
        };
        let digits = hex.trim_start_matches('0');
        if !hex.bytes().all(|b| b.is_ascii_hexdigit()) || digits.len() > 6 {
            return false;
        }
        let Some(c) = u32::from_str_radix(if digits.is_empty() { "0" } else { digits }, 16)
            .ok()
            .and_then(char::from_u32)
        else {
            return false;
        };
        let fits = if spelled.is_empty() {
            is_identifier_start(c)
        } else {
            is_identifier_part(c)
        };
        if !fits {
            return false;
        }
        spelled.push(c);
        rest = tail;
    }
    spelled.push_str(rest);
    spelled == name
}

#[cfg(test)]
mod tests {
    use super::escapes_spell;

    #[test]
    fn identifier_escapes() {
        assert!(escapes_spell("a\\u0041", "aA"));
        assert!(escapes_spell("\\u{000041}b\\u00e9", "Ab\u{e9}"));
        assert!(!escapes_spell("a\\u{1F600}", "a\u{1F600}"));
        assert!(escapes_spell("\\u{00000000000041}", "A"));
        assert!(!escapes_spell("a\\x41", "a41"));
        assert!(!escapes_spell("\\N", ""));
        assert!(!escapes_spell("\\u0030", "0"));
        assert!(escapes_spell("a\\u0030", "a0"));
        assert!(!escapes_spell("\\u{}", ""));
        assert!(!escapes_spell("\\u{110000}", ""));
        assert!(!escapes_spell("\\u004", ""));
        assert!(!escapes_spell("\\u0020", " "));
        assert!(!escapes_spell("\\u0041", "B"));
    }
}
