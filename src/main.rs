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
mod profiler;
mod lsp;
pub mod traits;

use crate::error::CompileError;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::Command;
use std::fs;

/// Nova language compiler — compiles .nv files to C++
#[derive(Parser)]
#[command(name = "nova")]
#[command(about = "Nova language compiler", version = "0.1.0")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Input .nv source file
    #[arg(value_name = "FILE")]
    input: Option<PathBuf>,

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

    /// Profile compiler phases and print timing report to stderr
    #[arg(long = "profile")]
    profile: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the LSP server for IDE integration
    Lsp,
    /// Compile a .nv file to C++ and run the resulting binary
    Run {
        /// Input .nv source file
        #[arg(value_name = "FILE")]
        input: PathBuf,

        /// Include the GC runtime header inline in the output
        #[arg(long = "standalone")]
        standalone: bool,

        /// Additional search paths for modules (can be specified multiple times)
        #[arg(short = 'L', long = "lib-path")]
        lib_paths: Vec<PathBuf>,

        /// C++ compiler to use (default: clang++)
        #[arg(long = "cxx", default_value = "clang++")]
        cxx: String,

        /// Keep the temporary C++ file and binary (don't delete after run)
        #[arg(long = "keep-temp")]
        keep_temp: bool,

        /// Profile compiler phases and print timing report to stderr
        #[arg(long = "profile")]
        profile: bool,
    },
    /// Interpret a .nv file directly without compiling to C++
    Interpret {
        /// Input .nv source file
        #[arg(value_name = "FILE")]
        input: PathBuf,

        /// Additional search paths for modules (can be specified multiple times)
        #[arg(short = 'L', long = "lib-path")]
        lib_paths: Vec<PathBuf>,

        /// Profile compiler phases and print timing report to stderr
        #[arg(long = "profile")]
        profile: bool,
    },
}

fn main() {
    let cli = Cli::parse();

    match &cli.command {
        Some(Commands::Run { input, standalone, lib_paths, cxx, keep_temp, profile }) => {
            if let Err(e) = run_nova(input, *standalone, lib_paths, &cxx, *keep_temp, *profile) {
                let source = fs::read_to_string(input).unwrap_or_default();
                eprintln!("{}", e.display_with_source(&source, &input.display().to_string()));
                std::process::exit(1);
            }
            return;
        }
        Some(Commands::Interpret { input, lib_paths, profile }) => {
            if let Err(e) = interpret(input, lib_paths, *profile) {
                let source = fs::read_to_string(input).unwrap_or_default();
                eprintln!("{}", e.display_with_source(&source, &input.display().to_string()));
                std::process::exit(1);
            }
            return;
        }
        Some(Commands::Lsp) => {
            let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
            rt.block_on(lsp::run());
            return;
        }
        None => {}
    }

    let Some(input) = cli.input.as_ref() else {
        eprintln!("error: FILE argument or subcommand required (try 'nova run <FILE>' or 'nova --help')");
        std::process::exit(1);
    };
    let file_path = input.display().to_string();

    if let Err(e) = run(&cli, cli.profile) {
        // Read source for error display
        let source = fs::read_to_string(input).unwrap_or_default();
        eprintln!("{}", e.display_with_source(&source, &file_path));
        std::process::exit(1);
    }
}

fn build_cpp(
    input: &PathBuf,
    source: &str,
    standalone: bool,
    lib_paths: &[PathBuf],
    module_name: &str,
    print_ast: bool,
    check_only: bool,
    prof: &profiler::Profiler,
) -> Result<Option<String>, CompileError> {
    // Setup search paths for @import
    let mut search_paths = lib_paths.to_vec();
    if let Some(parent) = input.parent() {
        search_paths.push(parent.to_path_buf());
    }
    search_paths.push(PathBuf::from("."));

    // Initialize import macro and interpreter
    let import_macro = import_macro::ImportMacro::new(search_paths.clone());
    let interpreter = interpreter::Interpreter::new(search_paths);

    // Lex
    prof.start("lex");
    let mut lex = lexer::Lexer::new(source);
    let tokens = lex.tokenize()?;
    prof.end();

    // Parse
    prof.start("parse");
    let mut p = parser::Parser::new(tokens, source);
    let mut module = p.parse_module(module_name.to_string())?;
    prof.end();

    if print_ast {
        println!("// ─── AST ───");
        println!("{:#?}", module);
        println!();
    }

    // Expand macros (including @import)
    prof.start("macro expand");
    let mut expander = macro_expand::MacroExpander::new(import_macro, interpreter);
    expander.register_structs(&module);
    expander.expand_module(&mut module, Some(prof))?;
    prof.end();

    if print_ast {
        println!("// ─── AST (after macro expansion) ───");
        println!("{:#?}", module);
        println!();
    }

    // Type check
    prof.start("type check");
    let mut checker = typeck::TypeChecker::new();
    checker.register_types(&module);
    let resolutions = checker.check_module(&module)?;
    prof.end();

    // Apply DotAccess resolutions (field vs UFCS call)
    prof.start("resolve");
    resolve::apply_resolutions(&mut module, &resolutions);
    prof.end();

    if check_only {
        println!("Type checking passed ✓");
        return Ok(None);
    }

    // Generate C++
    prof.start("codegen");
    let generator = codegen::cpp::CppGenerator::new();
    let cpp_output = generator.generate(&module);
    prof.end();

    let final_output = if standalone {
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

    Ok(Some(final_output))
}

fn run_nova(
    input: &PathBuf,
    standalone: bool,
    lib_paths: &[PathBuf],
    cxx: &str,
    keep_temp: bool,
    profile: bool,
) -> Result<(), CompileError> {
    let prof = profiler::Profiler::new(profile);

    prof.start("file read");
    let source = fs::read_to_string(input)?;
    prof.end();

    let module_name = input
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("main");

    let cpp_output = build_cpp(input, &source, standalone, lib_paths, module_name, false, false, &prof)?
        .ok_or_else(|| CompileError::Generic("Unexpected check-only in run mode".into()))?;

    // Write to temp C++ file
    let tmp_dir = std::env::temp_dir();
    let cpp_path = if keep_temp {
        input.with_extension("gen.cpp")
    } else {
        tmp_dir.join(format!("nova_{}.cpp", module_name))
    };
    fs::write(&cpp_path, &cpp_output)?;

    let bin_path = if keep_temp {
        input.with_extension("")
    } else {
        tmp_dir.join(format!("nova_{}", module_name))
    };

    // Compile with C++ compiler
    prof.start("c++ compile");
    eprintln!("Compiling {} with {}...", input.display(), cxx);
    let compile_status = Command::new(cxx)
        .arg("-std=c++20")
        .arg("-w")
        .arg("-x").arg("c++")
        .arg(&cpp_path)
        .arg("-o").arg(&bin_path)
        .status()
        .map_err(|e| CompileError::Generic(format!("Failed to run {}: {}", cxx, e)))?;
    prof.end();

    if !compile_status.success() {
        if !keep_temp {
            let _ = fs::remove_file(&cpp_path);
            let _ = fs::remove_file(&bin_path);
        }
        return Err(CompileError::Generic("C++ compilation failed".into()));
    }

    // Run the binary
    prof.start("execution");
    eprintln!("Running {}...", bin_path.display());
    let run_status = Command::new(&bin_path)
        .status()
        .map_err(|e| CompileError::Generic(format!("Failed to run binary: {}", e)))?;
    prof.end();

    prof.report();

    if !run_status.success() {
        // Cleanup
        if !keep_temp {
            let _ = fs::remove_file(&cpp_path);
            let _ = fs::remove_file(&bin_path);
        }
        std::process::exit(run_status.code().unwrap_or(1));
    }

    // Cleanup temp files
    if !keep_temp {
        let _ = fs::remove_file(&cpp_path);
        let _ = fs::remove_file(&bin_path);
    }

    Ok(())
}

fn run(cli: &Cli, profile: bool) -> Result<(), CompileError> {
    let prof = profiler::Profiler::new(profile);

    prof.start("file read");
    let input = cli.input.as_ref().unwrap();
    let source = fs::read_to_string(input)?;
    prof.end();

    let module_name = input
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("main")
        .to_string();

    let cpp_output = build_cpp(
        input, &source, cli.standalone, &cli.lib_paths, &module_name,
        cli.print_ast, cli.check_only, &prof,
    )?;

    prof.report();

    // check_only returns None (already printed "Type checking passed ✓")
    if let Some(output) = cpp_output {
        if let Some(ref out_path) = cli.output {
            fs::write(out_path, &output)?;
            eprintln!("Generated: {}", out_path.display());
        } else {
            println!("{}", output);
        }
    }

    Ok(())
}

fn interpret(input: &PathBuf, lib_paths: &[PathBuf], profile: bool) -> Result<(), CompileError> {
    let prof = profiler::Profiler::new(profile);

    prof.start("file read");
    let source = fs::read_to_string(input)?;
    prof.end();

    let module_name = input
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("main")
        .to_string();

    // Setup search paths
    let mut search_paths = lib_paths.to_vec();
    if let Some(parent) = input.parent() {
        search_paths.push(parent.to_path_buf());
    }
    search_paths.push(PathBuf::from("."));

    // Init import macro and interpreter (for macro expansion)
    let import_macro = import_macro::ImportMacro::new(search_paths.clone());
    let macro_interp = interpreter::Interpreter::new(search_paths.clone());

    // Lex
    prof.start("lex");
    let mut lex = lexer::Lexer::new(&source);
    let tokens = lex.tokenize()?;
    prof.end();

    // Parse
    prof.start("parse");
    let mut p = parser::Parser::new(tokens, &source);
    let mut module = p.parse_module(module_name.clone())?;
    prof.end();

    // Expand macros (including @import)
    prof.start("macro expand");
    let mut expander = macro_expand::MacroExpander::new(import_macro, macro_interp);
    expander.register_structs(&module);
    expander.expand_module(&mut module, Some(&prof))?;
    prof.end();

    // Type check (resolves DotAccess → field/UFCS, etc.)
    prof.start("type check");
    let mut checker = typeck::TypeChecker::new();
    checker.register_types(&module);
    let resolutions = checker.check_module(&module)?;
    prof.end();

    prof.start("resolve");
    resolve::apply_resolutions(&mut module, &resolutions);
    prof.end();

    // Create a fresh interpreter for runtime evaluation
    prof.start("interp setup");
    let mut rt = interpreter::Interpreter::new(search_paths);
    rt.enable_runtime_mode();
    rt.register_module(&module);
    prof.end();

    // Call main()
    prof.start("execution");
    let main_args: Vec<interpreter::Value> = vec![];
    if let Err(e) = rt.call_function("main", &main_args, crate::token::Span::zero()) {
        return Err(e);
    }
    prof.end();

    prof.report();

    Ok(())
}
