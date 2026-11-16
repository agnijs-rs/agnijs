//! The Oxc run: parse, early errors, the crate's refusals, transform,
//! print with a source map. Runs on the worker thread.

use std::path::{Path, PathBuf};

use oxc_allocator::Allocator;
use oxc_codegen::{Codegen, CodegenOptions};
use oxc_diagnostics::{OxcDiagnostic, Severity};
use oxc_parser::Parser;
use oxc_semantic::SemanticBuilder;
use oxc_span::{GetSpan, SourceType};
use oxc_transformer::{HelperLoaderMode, TransformOptions, Transformer};

use crate::position::LineIndex;
use crate::{
    check, helpers, Diagnostic, Error, JsxRuntime, Options, Output, SourceKind, MAX_DIAGNOSTICS,
};

/// A diagnostic before its position is computed.
struct Raw {
    start: u32,
    end: u32,
    message: String,
    help: Option<String>,
}

pub(crate) fn run(file: &str, source: &str, options: &Options) -> Result<Output, Error> {
    let from_oxc = |d: &OxcDiagnostic| {
        let (start, end) = d
            .labels
            .first()
            .map_or((0, 0), |l| (l.offset(), l.offset() + l.len()));
        Raw {
            start,
            end,
            message: d.message.to_string(),
            help: d.help.as_ref().map(|h| h.to_string()),
        }
    };
    // Positions are computed for the reported diagnostics only: hostile
    // input can raise one error per few bytes, on a single 1 MiB line.
    let fail = |mut raw: Vec<Raw>| {
        raw.sort_by_key(|d| (d.start, d.end));
        raw.truncate(MAX_DIAGNOSTICS);
        let lines = LineIndex::new(source);
        let diags = raw
            .into_iter()
            .map(|r| {
                let (line, column) = lines.position(r.start);
                Diagnostic {
                    file: file.to_string(),
                    message: r.message,
                    help: r.help,
                    start: r.start,
                    end: r.end,
                    line,
                    column,
                }
            })
            .collect();
        Err(Error::Syntax(diags))
    };

    let source_type = SourceType::ts()
        .with_module(options.kind == SourceKind::Module)
        .with_script(options.kind == SourceKind::Script)
        .with_jsx(options.jsx);
    let alloc = Allocator::default();
    let parsed = Parser::new(&alloc, source, source_type).parse();
    let errors: Vec<Raw> = parsed.diagnostics.errors().map(from_oxc).collect();
    if !errors.is_empty() {
        return fail(errors);
    }
    let mut program = parsed.program;

    let semantic = SemanticBuilder::new()
        .with_check_syntax_error(true)
        // The enum transform needs the evaluated member values.
        .with_enum_eval(true)
        .build(&program);
    let mut errors: Vec<Raw> = semantic
        .diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .map(from_oxc)
        .collect();
    let checked = check::check(&program, options);
    errors.extend(checked.refusals.into_iter().map(|r| Raw {
        start: r.span.start,
        end: r.span.end,
        message: r.message,
        help: r.help,
    }));
    // The lexer reports an HTML-like comment (`<!--`, Annex B) in a module,
    // but the TypeScript parser drops the error when it rewinds after a
    // `<`, and the printer keeps the comment, which no module may hold.
    if options.kind == SourceKind::Module {
        for c in &program.comments {
            let text = c.span.source_text(source);
            if text.starts_with("<!--") || text.starts_with("-->") {
                errors.push(Raw {
                    start: c.span.start,
                    end: c.span.start + if text.starts_with("<!--") { 4 } else { 3 },
                    message: "HTML comments are not allowed in modules".into(),
                    help: Some("use `//` or `/* */`, or transpile it as a script".into()),
                });
            }
        }
    }
    if !errors.is_empty() {
        return fail(errors);
    }

    let transform_options = transform_options(options);
    let transformed = Transformer::new(&alloc, Path::new(file), &transform_options)
        .build_with_scoping(semantic.semantic.into_scoping(), &mut program);
    let mut errors: Vec<Raw> = transformed.diagnostics.errors().map(from_oxc).collect();
    // Oxc parses `import` and `export` in a TypeScript script, where `tsc`
    // would emit CommonJS for them. What is left of them once the types
    // are gone cannot run as a script.
    if options.kind == SourceKind::Script {
        for span in program
            .body
            .iter()
            .filter_map(|s| s.as_module_declaration())
            .map(GetSpan::span)
        {
            errors.push(Raw {
                start: span.start,
                end: span.end,
                message: "import and export declarations need a module".into(),
                help: Some("transpile it as SourceKind::Module (a .ts or .mts file)".into()),
            });
        }
    }
    if !errors.is_empty() {
        return fail(errors);
    }

    // The printer indents each nested statement one more level, so nesting
    // n deep costs n squared bytes of indentation: half a million nested
    // blocks, which fit in 1 MiB, would print 256 GB. Real code indents a
    // few levels per statement; past four levels per source byte the
    // output is printed unindented, which bounds it by the input.
    let indent = checked.indent_levels <= 4 * source.len() as u64;
    let mut codegen_options = CodegenOptions {
        source_map_path: Some(PathBuf::from(file)),
        ..CodegenOptions::default()
    };
    if !indent {
        codegen_options.indent_width = 0;
    }
    let printed = Codegen::new()
        .with_options(codegen_options)
        .with_source_text(source)
        .build(&program);
    let mut code = printed.code;
    let mut source_map = printed
        .map
        .ok_or_else(|| Error::Internal("the printer returned no source map".into()))?
        .to_json_string();
    if checked.uses_decorators {
        let prologue = usize::from(program.hashbang.is_some()) + program.directives.len();
        helpers::insert_prelude(&mut code, &mut source_map, prologue);
    }
    Ok(Output { code, source_map })
}

fn transform_options(options: &Options) -> TransformOptions {
    // "esnext": nothing but TypeScript and JSX is lowered; the engine runs
    // the rest of the language as written.
    let mut t = TransformOptions::from_target("esnext").unwrap_or_default();
    t.helper_loader.mode = HelperLoaderMode::External;
    t.decorator.legacy = options.experimental_decorators;
    t.decorator.emit_decorator_metadata =
        options.experimental_decorators && options.emit_decorator_metadata;
    t.typescript.only_remove_type_imports = options.verbatim_module_syntax;
    t.typescript.jsx_pragma = options.jsx_factory.clone().into();
    t.typescript.jsx_pragma_frag = options.jsx_fragment_factory.clone().into();
    t.jsx.jsx_plugin = options.jsx;
    t.jsx.runtime = match options.jsx_runtime {
        JsxRuntime::Classic => oxc_transformer::JsxRuntime::Classic,
        JsxRuntime::Automatic => oxc_transformer::JsxRuntime::Automatic,
    };
    t.jsx.pragma = Some(options.jsx_factory.clone());
    t.jsx.pragma_frag = Some(options.jsx_fragment_factory.clone());
    t.jsx.import_source = Some(options.jsx_import_source.clone());
    t.jsx.development = false;
    t
}
