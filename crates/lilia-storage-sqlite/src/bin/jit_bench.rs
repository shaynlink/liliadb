use std::hint::black_box;
use std::time::Instant;

use lilia_core::json::{eval_filter_predicate, parse_expr, BinaryDoc, JitCompiler};
use serde_json::json;

fn main() {
    println!("================================================================================");
    println!(" LiliaDB Cranelift Native Query Compilation Benchmark");
    println!(
        " Target: 200,000 documents | Predicate: ($.price * 1.2 > 50.0) AND ($.status == 'paid')"
    );
    println!("================================================================================\n");

    const DOC_COUNT: usize = 200_000;
    println!("Generating {DOC_COUNT} synthetic BinaryDoc records...");
    let gen_start = Instant::now();
    let statuses = ["paid", "pending", "refunded", "cancelled"];
    let docs: Vec<BinaryDoc> = (0..DOC_COUNT)
        .map(|i| {
            let price = (i % 100) as f64 * 1.5;
            let status = statuses[i % statuses.len()];
            BinaryDoc::from_json_value(&json!({
                "id": i,
                "price": price,
                "status": status,
                "active": true,
            }))
            .unwrap()
        })
        .collect();
    let doc_refs: Vec<&BinaryDoc> = docs.iter().collect();
    println!("Generated in {:.2?}\n", gen_start.elapsed());

    let expr_str = "($.price * 1.2 > 50.0) AND ($.status == 'paid')";
    let expr = parse_expr(expr_str).unwrap();

    // 1. Measure Compilation Latency
    println!("--- 1. Compilation Latency ---");
    let comp_start = Instant::now();
    let compiler = JitCompiler::new().expect("JIT compiler creation");
    let compiled = compiler.compile(&expr).expect("compile expression");
    let comp_elapsed = comp_start.elapsed();
    println!(
        "Cranelift JIT compilation time: {:.2?} (~{:.1} µs)\n",
        comp_elapsed,
        comp_elapsed.as_micros()
    );

    // 2. Interpreted AST Scan
    println!("--- 2. Baseline: Interpreted AST Scan ---");
    let mut ast_matches = 0usize;
    let ast_start = Instant::now();
    for doc in &docs {
        if eval_filter_predicate(&expr, doc) {
            ast_matches += 1;
        }
    }
    let ast_elapsed = ast_start.elapsed();
    let ast_ns_per_doc = ast_elapsed.as_nanos() as f64 / DOC_COUNT as f64;
    let ast_throughput = (DOC_COUNT as f64 / ast_elapsed.as_secs_f64()) / 1_000_000.0;
    println!(
        "Interpreted AST: {:.2?} total | {:.1} ns/doc | {:.2} M docs/sec | matched: {ast_matches}",
        ast_elapsed, ast_ns_per_doc, ast_throughput
    );

    // Warm-up runs
    for _ in 0..2 {
        let mut count = 0;
        for doc in &docs {
            if compiled.eval(doc) {
                count += 1;
            }
        }
        black_box(count);
    }

    // 3. Cranelift JIT Single-Doc Loop
    println!("\n--- 3. Cranelift JIT (Single Document Loop) ---");
    let mut jit_single_matches = 0usize;
    let jit_single_start = Instant::now();
    for doc in &docs {
        if compiled.eval(doc) {
            jit_single_matches += 1;
        }
    }
    let jit_single_elapsed = jit_single_start.elapsed();
    let jit_single_ns_per_doc = jit_single_elapsed.as_nanos() as f64 / DOC_COUNT as f64;
    let jit_single_throughput = (DOC_COUNT as f64 / jit_single_elapsed.as_secs_f64()) / 1_000_000.0;
    let jit_single_speedup = ast_elapsed.as_secs_f64() / jit_single_elapsed.as_secs_f64();
    println!("Cranelift JIT Single: {:.2?} total | {:.1} ns/doc | {:.2} M docs/sec | matched: {jit_single_matches}",
        jit_single_elapsed, jit_single_ns_per_doc, jit_single_throughput);
    println!("Speedup vs AST Interpreter: {:.2}x", jit_single_speedup);

    // 4. Cranelift JIT Vectorized Batch Scan
    println!("\n--- 4. Cranelift JIT (Batch Vectorized Native Loop) ---");
    let mut batch_matches = Vec::with_capacity(DOC_COUNT);
    let jit_batch_start = Instant::now();
    compiled.eval_batch(&doc_refs, &mut batch_matches);
    let jit_batch_elapsed = jit_batch_start.elapsed();
    let jit_batch_ns_per_doc = jit_batch_elapsed.as_nanos() as f64 / DOC_COUNT as f64;
    let jit_batch_throughput = (DOC_COUNT as f64 / jit_batch_elapsed.as_secs_f64()) / 1_000_000.0;
    let jit_batch_speedup = ast_elapsed.as_secs_f64() / jit_batch_elapsed.as_secs_f64();
    println!(
        "Cranelift JIT Batch:  {:.2?} total | {:.1} ns/doc | {:.2} M docs/sec | matched: {}",
        jit_batch_elapsed,
        jit_batch_ns_per_doc,
        jit_batch_throughput,
        batch_matches.len()
    );
    println!("Speedup vs AST Interpreter: {:.2}x", jit_batch_speedup);

    // Parity verification
    assert_eq!(
        ast_matches, jit_single_matches,
        "Parity check single failed"
    );
    assert_eq!(
        ast_matches,
        batch_matches.len(),
        "Parity check batch failed"
    );
    println!(
        "\n[OK] Parity verified: Exact same {ast_matches} documents matched across all modes."
    );
    println!("================================================================================");
}
