//! The constructs the crate refuses before Oxc transforms anything: the
//! ones that would not run on the engine, and in [`Mode::StripOnly`] the
//! ones that are not erasable. Bodies of ambient declarations (`declare`,
//! `declare module "x"`, `declare global`) hold no code and are skipped.

use std::collections::{HashMap, HashSet};

use oxc_allocator::ArenaVec;
use oxc_ast::ast::{
    AccessorProperty, Class, ClassElement, Declaration, Decorator, FormalParameter,
    IdentifierReference, ImportOrExportKind, Program, Statement, TSEnumDeclaration,
    TSExportAssignment, TSExternalModuleDeclaration, TSGlobalDeclaration,
    TSImportEqualsDeclaration, TSModuleBlock, TSModuleReference, TSNamespaceDeclaration,
    TSNamespaceDeclarationBody,
};
use oxc_ast_visit::{walk, Visit};
use oxc_span::Span;

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
        refusals: Vec::new(),
        uses_decorators: false,
        depth: 0,
        indent_levels: 0,
        merged_blocks: HashMap::new(),
        merged: Vec::new(),
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
        self.indent_levels = self.indent_levels.saturating_add(self.depth);
        self.depth += 1;
        walk::walk_statement(self, it);
        self.depth -= 1;
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
        walk::walk_ts_enum_declaration(self, it);
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
        walk::walk_ts_namespace_declaration(self, it);
        if pushed {
            self.merged.pop();
        }
    }

    fn visit_statements(&mut self, it: &ArenaVec<'a, Statement<'a>>) {
        find_merged_namespaces(it, &mut self.merged_blocks);
        walk::walk_statements(self, it);
    }

    fn visit_identifier_reference(&mut self, it: &IdentifierReference<'a>) {
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
        walk::walk_class(self, it);
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
