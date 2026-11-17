use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use lilia_core::json::{eval_filter_predicate, parse_expr, BinaryDoc, JitCompiler};
use serde_json::json;
use std::hint::black_box;

fn bench_query_evaluation(c: &mut Criterion) {
    let mut group = c.benchmark_group("query_predicate_200k");
    const DOC_COUNT: usize = 200_000;
    group.throughput(Throughput::Elements(DOC_COUNT as u64));

    // Synthesize 200,000 documents
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

    // Query: (price * 1.2 > 50.0) && (status == 'paid')
    let expr_str = "($.price * 1.2 > 50.0) AND ($.status == 'paid')";
    let expr = parse_expr(expr_str).unwrap();

    let compiler = JitCompiler::new().expect("JIT compiler");
    let compiled = compiler.compile(&expr).expect("compile expression");

    // 1. Interpreted AST Baseline
    group.bench_function(BenchmarkId::new("mode", "interpreted_ast"), |b| {
        b.iter(|| {
            let mut matches = 0usize;
            for doc in &docs {
                if eval_filter_predicate(&expr, doc) {
                    matches += 1;
                }
            }
            black_box(matches);
        });
    });

    // 2. Cranelift JIT Single
    group.bench_function(BenchmarkId::new("mode", "cranelift_jit_single"), |b| {
        b.iter(|| {
            let mut matches = 0usize;
            for doc in &docs {
                if compiled.eval(doc) {
                    matches += 1;
                }
            }
            black_box(matches);
        });
    });

    // 3. Cranelift JIT Batch Vectorized
    group.bench_function(BenchmarkId::new("mode", "cranelift_jit_batch"), |b| {
        let mut matches = Vec::with_capacity(DOC_COUNT);
        b.iter(|| {
            matches.clear();
            compiled.eval_batch(&doc_refs, &mut matches);
            black_box(matches.len());
        });
    });

    group.finish();
}

criterion_group!(benches, bench_query_evaluation);
criterion_main!(benches);
