mod token;
mod ast;
mod lexer;
mod parser;
mod error;
mod macro_expand;
mod typeck;
mod codegen;
mod import_macro;
mod interpreter;
mod resolve;
pub mod traits;

use crate::error::CompileError;
use crate::ast::Item;
use clap::Parser;
use std::path::PathBuf;
use std::fs;

/// Nova language compiler — compiles .nv files to C++
#[derive(Parser)]
#[command(name = "nova")]
#[command(about = "Nova language compiler", version = "0.1.0")]
struct Cli {
    /// Input .nv source file
    #[arg(value_name = "FILE")]
    input: PathBuf,

    /// Output C++ file (default: stdout if -o not specified)
    #[arg(short = 'o', long = "output")]
    output: Option<PathBuf>,

    /// Print AST (debug)
    #[arg(long = "print-ast")]
    print_ast: bool,

    /// Run type checking only (no codegen)
    #[arg(long = "check-only")]
    check_only: bool,

    /// Include the GC runtime header inline in the output
    #[arg(long = "standalone")]
    standalone: bool,

    /// Additional search paths for modules (can be specified multiple times)
    #[arg(short = 'L', long = "lib-path")]
    lib_paths: Vec<PathBuf>,
}

fn main() {
    let cli = Cli::parse();
    let file_path = cli.input.display().to_string();

    if let Err(e) = run(&cli) {
        // Read source for error display
        let source = fs::read_to_string(&cli.input).unwrap_or_default();
        eprintln!("{}", e.display_with_source(&source, &file_path));
        std::process::exit(1);
    }
}

fn run(cli: &Cli) -> Result<(), CompileError> {
    let source = fs::read_to_string(&cli.input)?;
    let module_name = cli.input
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("main")
        .to_string();

    // Setup search paths for @import
    let mut search_paths = cli.lib_paths.clone();
    if let Some(parent) = cli.input.parent() {
        search_paths.push(parent.to_path_buf());
    }
    search_paths.push(PathBuf::from("."));

    // Initialize import macro and interpreter
    let import_macro = import_macro::ImportMacro::new(search_paths.clone());
    let interpreter = interpreter::Interpreter::new(search_paths);

    // Lex
    let mut lex = lexer::Lexer::new(&source);
    let tokens = lex.tokenize()?;

    // Parse
    let mut p = parser::Parser::new(tokens, &source);
    let mut module = p.parse_module(module_name.clone())?;

    if cli.print_ast {
        println!("// ─── AST ───");
        println!("{:#?}", module);
        println!();
    }

    // Expand macros (including @import)
    let mut expander = macro_expand::MacroExpander::new(import_macro, interpreter);
    // Register structs so macros can use lookup_type
    expander.register_structs(&module);
    expander.expand_module(&mut module)?;

    if cli.print_ast {
        println!("// ─── AST (after macro expansion) ───");
        println!("{:#?}", module);
        println!();
    }

    // Type check
    let mut checker = typeck::TypeChecker::new();
    checker.register_types(&module);
    let resolutions = checker.check_module(&module)?;
    // Apply DotAccess resolutions (field vs UFCS call)
    resolve::apply_resolutions(&mut module, &resolutions);

    if cli.check_only {
        println!("Type checking passed ✓");
        return Ok(());
    }

    // Generate C++
    let generator = codegen::cpp::CppGenerator::new();
    let cpp_output = generator.generate(&module);

    // Write output
    let final_output = if cli.standalone {
        let gc_rt_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("rt").join("gc.h");
        let gc_rt = if gc_rt_path.exists() {
            fs::read_to_string(&gc_rt_path).unwrap_or_default()
        } else {
            fs::read_to_string("rt/gc.h")
                .unwrap_or_else(|_| "// GC runtime not found\n".to_string())
        };
        format!("{}\n{}", gc_rt, cpp_output)
    } else {
        cpp_output
    };

    if let Some(ref out_path) = cli.output {
        fs::write(out_path, &final_output)?;
        eprintln!("Generated: {}", out_path.display());
    } else {
        println!("{}", final_output);
    }

    Ok(())
}
