#![allow(unsafe_code)]

use cranelift_codegen::ir::condcodes::{FloatCC, IntCC};
use cranelift_codegen::ir::{
    types, AbiParam, Block, FuncRef, InstBuilder, MemFlagsData, StackSlotData, StackSlotKind, Type,
    Value,
};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{Linkage, Module};

use super::helpers::{
    lilia_jit_extract_bool, lilia_jit_extract_f64, lilia_jit_extract_i64, lilia_jit_extract_str,
    lilia_jit_is_present_and_not_null, lilia_jit_memcmp, lilia_jit_strcmp, DocRef,
};
use super::predicate::CompiledPredicate;
use crate::json::expr::ast::{BinaryOp, Expr, LiteralValue, UnaryOp};
use crate::{ErrorCode, LiliaError, Result};

/// Determines whether an AST expression can be compiled by the Cranelift JIT engine.
#[must_use]
pub fn can_jit_expr(expr: &Expr) -> bool {
    match expr {
        Expr::Path(p) => !p.is_empty(),
        Expr::Literal(_) => true,
        Expr::Binary { op: _, left, right } => can_jit_expr(left) && can_jit_expr(right),
        Expr::Unary { op: _, expr } => can_jit_expr(expr),
        Expr::IsNull(inner) | Expr::IsNotNull(inner) => can_jit_expr(inner),
        Expr::FunctionCall { .. } => false,
    }
}

/// Cranelift JIT compiler for LiliaDB query filter expressions.
pub struct JitCompiler {
    module: JITModule,
    strings: Vec<String>,
}

impl std::fmt::Debug for JitCompiler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JitCompiler")
            .field("strings", &self.strings)
            .finish_non_exhaustive()
    }
}

impl JitCompiler {
    pub fn new() -> Result<Self> {
        let mut flag_builder = settings::builder();
        flag_builder.set("use_colocated_libcalls", "false").ok();
        flag_builder.set("is_pic", "false").ok();
        flag_builder.set("opt_level", "speed").ok();

        let isa_builder = cranelift_native::builder()
            .map_err(|e| LiliaError::new(ErrorCode::Storage, e.to_string(), false))?;
        let target_isa = isa_builder
            .finish(settings::Flags::new(flag_builder))
            .map_err(|e| LiliaError::new(ErrorCode::Storage, e.to_string(), false))?;

        let mut jit_builder =
            JITBuilder::with_isa(target_isa, cranelift_module::default_libcall_names());

        // Register runtime helper symbols
        jit_builder.symbol("lilia_jit_extract_f64", lilia_jit_extract_f64 as *const u8);
        jit_builder.symbol("lilia_jit_extract_i64", lilia_jit_extract_i64 as *const u8);
        jit_builder.symbol(
            "lilia_jit_extract_bool",
            lilia_jit_extract_bool as *const u8,
        );
        jit_builder.symbol("lilia_jit_extract_str", lilia_jit_extract_str as *const u8);
        jit_builder.symbol(
            "lilia_jit_is_present_and_not_null",
            lilia_jit_is_present_and_not_null as *const u8,
        );
        jit_builder.symbol("lilia_jit_memcmp", lilia_jit_memcmp as *const u8);
        jit_builder.symbol("lilia_jit_strcmp", lilia_jit_strcmp as *const u8);

        let module = JITModule::new(jit_builder);
        Ok(Self {
            module,
            strings: Vec::new(),
        })
    }

    /// Compiles an AST filter predicate into a [`CompiledPredicate`].
    pub fn compile(mut self, expr: &Expr) -> Result<CompiledPredicate> {
        if !can_jit_expr(expr) {
            return Err(LiliaError::new(
                ErrorCode::InvalidInput,
                "expression contains constructs not supported by JIT",
                false,
            ));
        }

        let ptr_type = self.module.isa().pointer_type();
        let frontend_config = self.module.isa().frontend_config();
        let mut ctx = self.module.make_context();
        let mut builder_context = FunctionBuilderContext::new();

        // 1. Declare and compile single-document function:
        // extern "C" fn(doc_ptr: *const u8, doc_len: usize) -> u8
        let mut sig_single = self.module.make_signature();
        sig_single.params.push(AbiParam::new(ptr_type));
        sig_single.params.push(AbiParam::new(ptr_type));
        sig_single.returns.push(AbiParam::new(types::I8));

        let single_fn_id = self
            .module
            .declare_function("jit_filter_single", Linkage::Local, &sig_single)
            .map_err(|e| LiliaError::new(ErrorCode::Storage, e.to_string(), false))?;

        ctx.func.signature = sig_single;
        {
            let mut builder = FunctionBuilder::new(&mut ctx.func, &mut builder_context);
            let entry = builder.create_block();
            builder.append_block_params_for_function_params(entry);
            builder.switch_to_block(entry);
            builder.seal_block(entry);

            let doc_ptr = builder.block_params(entry)[0];
            let doc_len = builder.block_params(entry)[1];

            let false_block = builder.create_block();
            let true_block = builder.create_block();

            self.emit_bool_cond(
                &mut builder,
                expr,
                doc_ptr,
                doc_len,
                true_block,
                false_block,
            )?;

            builder.switch_to_block(true_block);
            builder.seal_block(true_block);
            let one = builder.ins().iconst(types::I8, 1);
            builder.ins().return_(&[one]);

            builder.switch_to_block(false_block);
            builder.seal_block(false_block);
            let zero = builder.ins().iconst(types::I8, 0);
            builder.ins().return_(&[zero]);

            builder.finalize(frontend_config);
        }

        self.module
            .define_function(single_fn_id, &mut ctx)
            .map_err(|e| {
                LiliaError::new(
                    ErrorCode::Storage,
                    format!("Single func compilation error: {e:?}"),
                    false,
                )
            })?;
        self.module.clear_context(&mut ctx);

        // 2. Declare and compile batch vectorized scan function:
        // extern "C" fn(docs: *const DocRef, count: usize, matches_out: *mut u32) -> usize
        let mut sig_batch = self.module.make_signature();
        sig_batch.params.push(AbiParam::new(ptr_type)); // docs
        sig_batch.params.push(AbiParam::new(ptr_type)); // count
        sig_batch.params.push(AbiParam::new(ptr_type)); // matches_out
        sig_batch.returns.push(AbiParam::new(ptr_type)); // returns match count

        let batch_fn_id = self
            .module
            .declare_function("jit_filter_batch", Linkage::Local, &sig_batch)
            .map_err(|e| LiliaError::new(ErrorCode::Storage, e.to_string(), false))?;

        ctx.func.signature = sig_batch;
        {
            let mut builder = FunctionBuilder::new(&mut ctx.func, &mut builder_context);
            let entry = builder.create_block();
            builder.append_block_params_for_function_params(entry);
            builder.switch_to_block(entry);
            builder.seal_block(entry);

            let docs_ptr = builder.block_params(entry)[0];
            let count = builder.block_params(entry)[1];
            let matches_out = builder.block_params(entry)[2];

            let var_i = builder.declare_var(ptr_type);
            let var_match_count = builder.declare_var(ptr_type);

            let zero_ptr = builder.ins().iconst(ptr_type, 0);
            let one_ptr = builder.ins().iconst(ptr_type, 1);
            let four = builder.ins().iconst(ptr_type, 4);
            let doc_ref_size = builder.ins().iconst(ptr_type, 16);

            builder.def_var(var_i, zero_ptr);
            builder.def_var(var_match_count, zero_ptr);

            let loop_header = builder.create_block();
            let loop_body = builder.create_block();
            let matched_block = builder.create_block();
            let next_iter = builder.create_block();
            let exit_block = builder.create_block();

            builder.ins().jump(loop_header, &[]);

            // loop_header
            builder.switch_to_block(loop_header);
            let curr_i = builder.use_var(var_i);
            let at_end = builder.ins().icmp(IntCC::Equal, curr_i, count);
            builder.ins().brif(at_end, exit_block, &[], loop_body, &[]);

            // loop_body
            builder.switch_to_block(loop_body);
            builder.seal_block(loop_body);

            // DocRef is 16 bytes: [0..8] = ptr, [8..16] = len
            let offset = builder.ins().imul(curr_i, doc_ref_size);
            let doc_ref_addr = builder.ins().iadd(docs_ptr, offset);

            let d_ptr = builder
                .ins()
                .load(ptr_type, MemFlagsData::trusted(), doc_ref_addr, 0);
            let d_len = builder
                .ins()
                .load(ptr_type, MemFlagsData::trusted(), doc_ref_addr, 8);

            // Call single filter function
            let local_single_fn = self.module.declare_func_in_func(single_fn_id, builder.func);
            let call_res = builder.ins().call(local_single_fn, &[d_ptr, d_len]);
            let is_matched = builder.inst_results(call_res)[0];

            let is_true = builder.ins().icmp_imm_u(IntCC::NotEqual, is_matched, 0);
            builder
                .ins()
                .brif(is_true, matched_block, &[], next_iter, &[]);

            // matched_block: write i as u32 into matches_out[match_count]
            builder.switch_to_block(matched_block);
            builder.seal_block(matched_block);

            let curr_match_count = builder.use_var(var_match_count);
            let out_offset = builder.ins().imul(curr_match_count, four);
            let write_addr = builder.ins().iadd(matches_out, out_offset);
            let i_u32 = builder.ins().ireduce(types::I32, curr_i);
            builder
                .ins()
                .store(MemFlagsData::trusted(), i_u32, write_addr, 0);

            let next_match_count = builder.ins().iadd(curr_match_count, one_ptr);
            let next_i = builder.ins().iadd(curr_i, one_ptr);
            builder.def_var(var_match_count, next_match_count);
            builder.def_var(var_i, next_i);
            builder.ins().jump(loop_header, &[]);

            // next_iter (not matched)
            builder.switch_to_block(next_iter);
            builder.seal_block(next_iter);
            let next_i_n = builder.ins().iadd(curr_i, one_ptr);
            builder.def_var(var_i, next_i_n);
            builder.ins().jump(loop_header, &[]);

            builder.seal_block(loop_header);

            // exit_block
            builder.switch_to_block(exit_block);
            builder.seal_block(exit_block);
            let final_matches = builder.use_var(var_match_count);
            builder.ins().return_(&[final_matches]);

            builder.finalize(frontend_config);
        }

        self.module
            .define_function(batch_fn_id, &mut ctx)
            .map_err(|e| {
                LiliaError::new(
                    ErrorCode::Storage,
                    format!("Batch func compilation error: {e:?}"),
                    false,
                )
            })?;
        self.module.clear_context(&mut ctx);

        self.module
            .finalize_definitions()
            .map_err(|e| LiliaError::new(ErrorCode::Storage, e.to_string(), false))?;

        let single_code = self.module.get_finalized_function(single_fn_id);
        let batch_code = self.module.get_finalized_function(batch_fn_id);

        let eval_fn = unsafe {
            std::mem::transmute::<*const u8, unsafe extern "C" fn(*const u8, usize) -> u8>(
                single_code,
            )
        };
        let batch_fn = unsafe {
            std::mem::transmute::<
                *const u8,
                unsafe extern "C" fn(*const DocRef, usize, *mut u32) -> usize,
            >(batch_code)
        };

        Ok(CompiledPredicate::new(
            eval_fn,
            batch_fn,
            self.strings,
            self.module,
        ))
    }

    /// Stores a static string in the string pool and returns (ptr, len) as Cranelift constants.
    fn alloc_const_str(&mut self, builder: &mut FunctionBuilder, s: &str) -> (Value, Value) {
        let ptr_type = self.module.isa().pointer_type();
        self.strings.push(s.to_string());
        let pooled = self.strings.last().unwrap();
        let ptr_val = pooled.as_ptr() as usize as i64;
        let len_val = pooled.len() as i64;
        let ptr = builder.ins().iconst(ptr_type, ptr_val);
        let len = builder.ins().iconst(ptr_type, len_val);
        (ptr, len)
    }

    /// Emits a boolean condition expression with short-circuiting branches.
    fn emit_bool_cond(
        &mut self,
        builder: &mut FunctionBuilder,
        expr: &Expr,
        doc_ptr: Value,
        doc_len: Value,
        true_dest: Block,
        false_dest: Block,
    ) -> Result<()> {
        match expr {
            Expr::Binary {
                op: BinaryOp::And,
                left,
                right,
            } => {
                let right_block = builder.create_block();
                self.emit_bool_cond(builder, left, doc_ptr, doc_len, right_block, false_dest)?;
                builder.switch_to_block(right_block);
                builder.seal_block(right_block);
                self.emit_bool_cond(builder, right, doc_ptr, doc_len, true_dest, false_dest)?;
            }
            Expr::Binary {
                op: BinaryOp::Or,
                left,
                right,
            } => {
                let right_block = builder.create_block();
                self.emit_bool_cond(builder, left, doc_ptr, doc_len, true_dest, right_block)?;
                builder.switch_to_block(right_block);
                builder.seal_block(right_block);
                self.emit_bool_cond(builder, right, doc_ptr, doc_len, true_dest, false_dest)?;
            }
            Expr::Unary {
                op: UnaryOp::Not,
                expr: inner,
            } => {
                self.emit_bool_cond(builder, inner, doc_ptr, doc_len, false_dest, true_dest)?;
            }
            Expr::IsNull(inner) => {
                if let Expr::Path(path) = inner.as_ref() {
                    let clean = path.trim_start_matches("$.").trim_start_matches('/');
                    let (path_ptr, path_len) = self.alloc_const_str(builder, clean);
                    let helper_fn = self.declare_helper(
                        builder,
                        "lilia_jit_is_present_and_not_null",
                        &[
                            builder.func.dfg.value_type(doc_ptr),
                            builder.func.dfg.value_type(doc_len),
                            builder.func.dfg.value_type(path_ptr),
                            builder.func.dfg.value_type(path_len),
                        ],
                        types::I8,
                    )?;

                    let call_inst = builder
                        .ins()
                        .call(helper_fn, &[doc_ptr, doc_len, path_ptr, path_len]);
                    let res = builder.inst_results(call_inst)[0];
                    let is_null = builder.ins().icmp_imm_u(IntCC::Equal, res, 0);
                    builder.ins().brif(is_null, true_dest, &[], false_dest, &[]);
                } else {
                    return Err(LiliaError::new(
                        ErrorCode::InvalidInput,
                        "unsupported IS NULL operand",
                        false,
                    ));
                }
            }
            Expr::IsNotNull(inner) => {
                if let Expr::Path(path) = inner.as_ref() {
                    let clean = path.trim_start_matches("$.").trim_start_matches('/');
                    let (path_ptr, path_len) = self.alloc_const_str(builder, clean);
                    let helper_fn = self.declare_helper(
                        builder,
                        "lilia_jit_is_present_and_not_null",
                        &[
                            builder.func.dfg.value_type(doc_ptr),
                            builder.func.dfg.value_type(doc_len),
                            builder.func.dfg.value_type(path_ptr),
                            builder.func.dfg.value_type(path_len),
                        ],
                        types::I8,
                    )?;

                    let call_inst = builder
                        .ins()
                        .call(helper_fn, &[doc_ptr, doc_len, path_ptr, path_len]);
                    let res = builder.inst_results(call_inst)[0];
                    let is_not_null = builder.ins().icmp_imm_u(IntCC::NotEqual, res, 0);
                    builder
                        .ins()
                        .brif(is_not_null, true_dest, &[], false_dest, &[]);
                } else {
                    return Err(LiliaError::new(
                        ErrorCode::InvalidInput,
                        "unsupported IS NOT NULL operand",
                        false,
                    ));
                }
            }
            Expr::Binary { op, left, right } if is_comparison_op(*op) => {
                self.emit_comparison(
                    builder, *op, left, right, doc_ptr, doc_len, true_dest, false_dest,
                )?;
            }
            Expr::Literal(LiteralValue::Bool(b)) => {
                if *b {
                    builder.ins().jump(true_dest, &[]);
                } else {
                    builder.ins().jump(false_dest, &[]);
                }
            }
            Expr::Path(path) => {
                // Standalone boolean path check (e.g. `$.active`)
                let clean = path.trim_start_matches("$.").trim_start_matches('/');
                let (path_ptr, path_len) = self.alloc_const_str(builder, clean);
                let ptr_type = self.module.isa().pointer_type();

                let ss = builder.create_sized_stack_slot(StackSlotData::new(
                    StackSlotKind::ExplicitSlot,
                    1,
                    0,
                ));
                let out_ptr = builder.ins().stack_addr(ptr_type, ss, 0);

                let helper_fn = self.declare_helper(
                    builder,
                    "lilia_jit_extract_bool",
                    &[ptr_type, ptr_type, ptr_type, ptr_type, ptr_type],
                    types::I8,
                )?;

                let call_inst = builder
                    .ins()
                    .call(helper_fn, &[doc_ptr, doc_len, path_ptr, path_len, out_ptr]);
                let found = builder.inst_results(call_inst)[0];
                let is_found = builder.ins().icmp_imm_u(IntCC::NotEqual, found, 0);

                let check_val_block = builder.create_block();
                builder
                    .ins()
                    .brif(is_found, check_val_block, &[], false_dest, &[]);

                builder.switch_to_block(check_val_block);
                builder.seal_block(check_val_block);
                let bool_val = builder
                    .ins()
                    .load(types::I8, MemFlagsData::trusted(), out_ptr, 0);
                let is_true = builder.ins().icmp_imm_u(IntCC::NotEqual, bool_val, 0);
                builder.ins().brif(is_true, true_dest, &[], false_dest, &[]);
            }
            _ => {
                return Err(LiliaError::new(
                    ErrorCode::InvalidInput,
                    "unsupported expression in JIT bool condition",
                    false,
                ));
            }
        }
        Ok(())
    }

    /// Emits a comparison operator (`==`, `!=`, `<`, `<=`, `>`, `>=`).
    #[allow(clippy::too_many_arguments)]
    fn emit_comparison(
        &mut self,
        builder: &mut FunctionBuilder,
        op: BinaryOp,
        left: &Expr,
        right: &Expr,
        doc_ptr: Value,
        doc_len: Value,
        true_dest: Block,
        false_dest: Block,
    ) -> Result<()> {
        let ptr_type = self.module.isa().pointer_type();

        // 1. Check if comparing strings
        let is_string_comp = matches!(left, Expr::Literal(LiteralValue::String(_)))
            || matches!(right, Expr::Literal(LiteralValue::String(_)));

        if is_string_comp {
            let (str_path, lit_str) = match (left, right) {
                (Expr::Path(p), Expr::Literal(LiteralValue::String(s))) => (p, s),
                (Expr::Literal(LiteralValue::String(s)), Expr::Path(p)) => (p, s),
                _ => {
                    return Err(LiliaError::new(
                        ErrorCode::InvalidInput,
                        "unsupported string comparison",
                        false,
                    ));
                }
            };

            let clean = str_path.trim_start_matches("$.").trim_start_matches('/');
            let (path_ptr, path_len) = self.alloc_const_str(builder, clean);
            let (lit_ptr, lit_len) = self.alloc_const_str(builder, lit_str);

            let ss_p = builder.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                8,
                3,
            ));
            let ss_l = builder.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                8,
                3,
            ));
            let out_p = builder.ins().stack_addr(ptr_type, ss_p, 0);
            let out_l = builder.ins().stack_addr(ptr_type, ss_l, 0);

            let helper_fn = self.declare_helper(
                builder,
                "lilia_jit_extract_str",
                &[ptr_type, ptr_type, ptr_type, ptr_type, ptr_type, ptr_type],
                types::I8,
            )?;

            let call_inst = builder.ins().call(
                helper_fn,
                &[doc_ptr, doc_len, path_ptr, path_len, out_p, out_l],
            );
            let found = builder.inst_results(call_inst)[0];
            let is_found = builder.ins().icmp_imm_u(IntCC::NotEqual, found, 0);

            let check_bytes_block = builder.create_block();
            builder
                .ins()
                .brif(is_found, check_bytes_block, &[], false_dest, &[]);

            builder.switch_to_block(check_bytes_block);
            builder.seal_block(check_bytes_block);
            let extracted_p = builder
                .ins()
                .load(ptr_type, MemFlagsData::trusted(), out_p, 0);
            let extracted_l = builder
                .ins()
                .load(ptr_type, MemFlagsData::trusted(), out_l, 0);

            if op == BinaryOp::Eq || op == BinaryOp::NotEq {
                let cmp_len = builder.ins().icmp(IntCC::Equal, extracted_l, lit_len);
                let check_content = builder.create_block();
                builder.ins().brif(
                    cmp_len,
                    check_content,
                    &[],
                    if op == BinaryOp::Eq {
                        false_dest
                    } else {
                        true_dest
                    },
                    &[],
                );

                builder.switch_to_block(check_content);
                builder.seal_block(check_content);
                let memcmp_fn = self.declare_helper(
                    builder,
                    "lilia_jit_memcmp",
                    &[ptr_type, ptr_type, ptr_type],
                    types::I32,
                )?;
                let call_memcmp = builder
                    .ins()
                    .call(memcmp_fn, &[extracted_p, lit_ptr, lit_len]);
                let memcmp_res = builder.inst_results(call_memcmp)[0];

                let is_eq = builder.ins().icmp_imm_u(IntCC::Equal, memcmp_res, 0);
                if op == BinaryOp::Eq {
                    builder.ins().brif(is_eq, true_dest, &[], false_dest, &[]);
                } else {
                    builder.ins().brif(is_eq, false_dest, &[], true_dest, &[]);
                }
            } else {
                let strcmp_fn = self.declare_helper(
                    builder,
                    "lilia_jit_strcmp",
                    &[ptr_type, ptr_type, ptr_type, ptr_type],
                    types::I32,
                )?;
                let call_strcmp = builder
                    .ins()
                    .call(strcmp_fn, &[extracted_p, extracted_l, lit_ptr, lit_len]);
                let cmp_res = builder.inst_results(call_strcmp)[0];

                let int_cc = match op {
                    BinaryOp::Lt => IntCC::SignedLessThan,
                    BinaryOp::Lte => IntCC::SignedLessThanOrEqual,
                    BinaryOp::Gt => IntCC::SignedGreaterThan,
                    BinaryOp::Gte => IntCC::SignedGreaterThanOrEqual,
                    _ => unreachable!(),
                };
                let matches_cond = builder.ins().icmp_imm_s(int_cc, cmp_res, 0);
                builder
                    .ins()
                    .brif(matches_cond, true_dest, &[], false_dest, &[]);
            }
            return Ok(());
        }

        // 2. Check if comparing booleans
        let is_bool_comp = matches!(left, Expr::Literal(LiteralValue::Bool(_)))
            || matches!(right, Expr::Literal(LiteralValue::Bool(_)));

        if is_bool_comp {
            let (bool_path, lit_bool) = match (left, right) {
                (Expr::Path(p), Expr::Literal(LiteralValue::Bool(b))) => (p, *b),
                (Expr::Literal(LiteralValue::Bool(b)), Expr::Path(p)) => (p, *b),
                _ => {
                    return Err(LiliaError::new(
                        ErrorCode::InvalidInput,
                        "unsupported boolean comparison",
                        false,
                    ));
                }
            };

            let clean = bool_path.trim_start_matches("$.").trim_start_matches('/');
            let (path_ptr, path_len) = self.alloc_const_str(builder, clean);

            let ss = builder.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                1,
                0,
            ));
            let out_ptr = builder.ins().stack_addr(ptr_type, ss, 0);

            let helper_fn = self.declare_helper(
                builder,
                "lilia_jit_extract_bool",
                &[ptr_type, ptr_type, ptr_type, ptr_type, ptr_type],
                types::I8,
            )?;

            let call_inst = builder
                .ins()
                .call(helper_fn, &[doc_ptr, doc_len, path_ptr, path_len, out_ptr]);
            let found = builder.inst_results(call_inst)[0];
            let is_found = builder.ins().icmp_imm_u(IntCC::NotEqual, found, 0);

            let check_val_block = builder.create_block();
            builder
                .ins()
                .brif(is_found, check_val_block, &[], false_dest, &[]);

            builder.switch_to_block(check_val_block);
            builder.seal_block(check_val_block);
            let bool_val = builder
                .ins()
                .load(types::I8, MemFlagsData::trusted(), out_ptr, 0);
            let target_imm: i64 = if lit_bool { 1 } else { 0 };
            let matches_cond = if op == BinaryOp::Eq {
                builder.ins().icmp_imm_u(IntCC::Equal, bool_val, target_imm)
            } else if op == BinaryOp::NotEq {
                builder
                    .ins()
                    .icmp_imm_u(IntCC::NotEqual, bool_val, target_imm)
            } else {
                return Err(LiliaError::new(
                    ErrorCode::InvalidInput,
                    "relational comparison (<, >, <=, >=) not supported on booleans",
                    false,
                ));
            };

            builder
                .ins()
                .brif(matches_cond, true_dest, &[], false_dest, &[]);
            return Ok(());
        }

        // 3. Numeric / Floating point comparison
        let left_val = self.emit_numeric_expr(builder, left, doc_ptr, doc_len, false_dest)?;
        let right_val = self.emit_numeric_expr(builder, right, doc_ptr, doc_len, false_dest)?;

        let float_cc = match op {
            BinaryOp::Eq => FloatCC::Equal,
            BinaryOp::NotEq => FloatCC::NotEqual,
            BinaryOp::Lt => FloatCC::LessThan,
            BinaryOp::Lte => FloatCC::LessThanOrEqual,
            BinaryOp::Gt => FloatCC::GreaterThan,
            BinaryOp::Gte => FloatCC::GreaterThanOrEqual,
            _ => unreachable!(),
        };

        let cmp = builder.ins().fcmp(float_cc, left_val, right_val);
        builder.ins().brif(cmp, true_dest, &[], false_dest, &[]);
        Ok(())
    }

    /// Evaluates an arithmetic expression, producing a `types::F64` value.
    fn emit_numeric_expr(
        &mut self,
        builder: &mut FunctionBuilder,
        expr: &Expr,
        doc_ptr: Value,
        doc_len: Value,
        fail_dest: Block,
    ) -> Result<Value> {
        let ptr_type = self.module.isa().pointer_type();

        match expr {
            Expr::Literal(LiteralValue::Float(f)) => Ok(builder.ins().f64const(*f)),
            Expr::Literal(LiteralValue::Int(i)) => Ok(builder.ins().f64const(*i as f64)),
            Expr::Path(path) => {
                let clean = path.trim_start_matches("$.").trim_start_matches('/');
                let (path_ptr, path_len) = self.alloc_const_str(builder, clean);

                let ss = builder.create_sized_stack_slot(StackSlotData::new(
                    StackSlotKind::ExplicitSlot,
                    8,
                    3,
                ));
                let out_ptr = builder.ins().stack_addr(ptr_type, ss, 0);

                let helper_fn = self.declare_helper(
                    builder,
                    "lilia_jit_extract_f64",
                    &[ptr_type, ptr_type, ptr_type, ptr_type, ptr_type],
                    types::I8,
                )?;

                let call_inst = builder
                    .ins()
                    .call(helper_fn, &[doc_ptr, doc_len, path_ptr, path_len, out_ptr]);
                let found = builder.inst_results(call_inst)[0];
                let is_found = builder.ins().icmp_imm_u(IntCC::NotEqual, found, 0);

                let ok_block = builder.create_block();
                builder.ins().brif(is_found, ok_block, &[], fail_dest, &[]);

                builder.switch_to_block(ok_block);
                builder.seal_block(ok_block);
                Ok(builder
                    .ins()
                    .load(types::F64, MemFlagsData::trusted(), out_ptr, 0))
            }
            Expr::Binary { op, left, right } => {
                let l = self.emit_numeric_expr(builder, left, doc_ptr, doc_len, fail_dest)?;
                let r = self.emit_numeric_expr(builder, right, doc_ptr, doc_len, fail_dest)?;
                match op {
                    BinaryOp::Add => Ok(builder.ins().fadd(l, r)),
                    BinaryOp::Sub => Ok(builder.ins().fsub(l, r)),
                    BinaryOp::Mul => Ok(builder.ins().fmul(l, r)),
                    BinaryOp::Div => {
                        // Division by zero check -> jump to fail_dest
                        let zero = builder.ins().f64const(0.0);
                        let is_zero = builder.ins().fcmp(FloatCC::Equal, r, zero);
                        let div_ok = builder.create_block();
                        builder.ins().brif(is_zero, fail_dest, &[], div_ok, &[]);

                        builder.switch_to_block(div_ok);
                        builder.seal_block(div_ok);
                        Ok(builder.ins().fdiv(l, r))
                    }
                    _ => Err(LiliaError::new(
                        ErrorCode::InvalidInput,
                        "unsupported operator in arithmetic expr",
                        false,
                    )),
                }
            }
            Expr::Unary {
                op: UnaryOp::Neg,
                expr,
            } => {
                let v = self.emit_numeric_expr(builder, expr, doc_ptr, doc_len, fail_dest)?;
                Ok(builder.ins().fneg(v))
            }
            _ => Err(LiliaError::new(
                ErrorCode::InvalidInput,
                "unsupported numeric expression",
                false,
            )),
        }
    }

    /// Declares an external helper function in the current FunctionBuilder.
    fn declare_helper(
        &mut self,
        builder: &mut FunctionBuilder,
        name: &str,
        params: &[Type],
        ret: Type,
    ) -> Result<FuncRef> {
        let mut sig = self.module.make_signature();
        for &p in params {
            sig.params.push(AbiParam::new(p));
        }
        sig.returns.push(AbiParam::new(ret));

        let func_id = self
            .module
            .declare_function(name, Linkage::Import, &sig)
            .map_err(|e| LiliaError::new(ErrorCode::Storage, e.to_string(), false))?;

        Ok(self.module.declare_func_in_func(func_id, builder.func))
    }
}

fn is_comparison_op(op: BinaryOp) -> bool {
    matches!(
        op,
        BinaryOp::Eq
            | BinaryOp::NotEq
            | BinaryOp::Lt
            | BinaryOp::Lte
            | BinaryOp::Gt
            | BinaryOp::Gte
    )
}
