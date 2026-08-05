//! What a worm rule costs per node, which decides how it should be scheduled
//!
//! Run with: cargo test --release --test worm_cost -- --nocapture --ignored

use std::sync::Arc;
use std::time::Instant;

use larvae::syntax::{lexer, parser};
use larvae::worm::WasmWorm;
use larvae::worm::ctx::FileCtx;
use larvae::worm::luau::LuauWorm;
use larvae::worm::nodes::{Kind, NodeTable};

const FIXTURE: &[u8] = include_bytes!("fixtures/echo_worm.wasm");

/// A file of roughly the shape a real one has
fn source(lines: usize) -> String {
    (0..lines)
        .map(|i| format!("local v{i} = compute({i}, \"text\")\n"))
        .collect()
}

fn file(src: &str) -> Arc<FileCtx> {
    let lexed = lexer::lex(src).unwrap();
    let chunk = parser::parse(src, &lexed.toks).unwrap();
    let toks = lexed.toks.clone();

    let bytes = move |span: larvae::syntax::ast::TokSpan| -> (u32, u32) {
        if span.is_empty() {
            let at = toks.get(span.start as usize).map(|t| t.start).unwrap_or(0);
            return (at, at);
        }
        (
            toks[span.start as usize].start,
            toks[span.end as usize - 1].end,
        )
    };

    Arc::new(FileCtx::new(
        NodeTable::build(&chunk, &bytes),
        src.to_owned(),
        "bench.luau".into(),
    ))
}

fn matched(f: &FileCtx) -> Vec<u32> {
    f.table
        .iter()
        .filter(|(_, n)| n.kind == Kind::CallExpr)
        .map(|(id, _)| id)
        .collect()
}

#[test]
#[ignore = "timing, run explicitly"]
fn what_a_rule_costs_per_node() {
    const FILES: usize = 300;
    const LINES: usize = 120;

    let src = source(LINES);
    let f = file(&src);
    let ids = matched(&f);

    println!("\n{} nodes matched per file, {FILES} files\n", ids.len());

    // parse alone, the cost a rule forces on a require only build
    let start = Instant::now();
    for _ in 0..FILES {
        let lexed = lexer::lex(&src).unwrap();
        let chunk = parser::parse(&src, &lexed.toks).unwrap();
        std::hint::black_box(&chunk);
    }
    let parse = start.elapsed();

    // flattening, which a rule also forces
    let lexed = lexer::lex(&src).unwrap();
    let chunk = parser::parse(&src, &lexed.toks).unwrap();
    let toks = lexed.toks.clone();
    let bytes = move |span: larvae::syntax::ast::TokSpan| -> (u32, u32) {
        if span.is_empty() {
            let at = toks.get(span.start as usize).map(|t| t.start).unwrap_or(0);
            return (at, at);
        }
        (
            toks[span.start as usize].start,
            toks[span.end as usize - 1].end,
        )
    };

    let start = Instant::now();
    for _ in 0..FILES {
        std::hint::black_box(NodeTable::build(&chunk, &bytes));
    }
    let flatten = start.elapsed();

    let mut luau = LuauWorm::load(
        r#"
return { rules = { r = { visit = function(node, ctx)
    if node:kind() == "CallExpr" then ctx:replace(node, node:text()) end
end } } }
"#,
        "bench",
    )
    .unwrap();

    let start = Instant::now();
    for _ in 0..FILES {
        let f = file(&src);
        let ids = matched(&f);
        std::hint::black_box(luau.run_rule("r", Arc::clone(&f), &ids).unwrap());
    }
    let luau_total = start.elapsed();

    let mut wasm = WasmWorm::load(FIXTURE).unwrap();

    let start = Instant::now();
    for _ in 0..FILES {
        let f = file(&src);
        let ids = matched(&f);
        std::hint::black_box(wasm.run_rule(0, Arc::clone(&f), &ids).unwrap());
    }
    let wasm_total = start.elapsed();

    let per = |d: std::time::Duration| d.as_secs_f64() * 1000.0 / FILES as f64;

    println!("  parse only          {:>8.3} ms/file", per(parse));
    println!("  flatten only        {:>8.3} ms/file", per(flatten));
    println!(
        "  luau rule, total    {:>8.3} ms/file  (includes lex+parse+flatten)",
        per(luau_total)
    );
    println!(
        "  wasm rule, total    {:>8.3} ms/file  (includes lex+parse+flatten)",
        per(wasm_total)
    );
    println!();
    println!(
        "  {FILES} files serial, luau: {:>6.1} ms",
        luau_total.as_secs_f64() * 1000.0
    );
    println!(
        "  {FILES} files serial, wasm: {:>6.1} ms",
        wasm_total.as_secs_f64() * 1000.0
    );
    println!();
}
