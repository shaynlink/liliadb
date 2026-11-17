use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use lilia_core::{ErrorCode, LiliaError, Result};
use lilia_sqlite_native::{NativeConnection, NativeContext, NativeValue};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WasmUdfConfig {
    pub memory_limit_bytes: usize,
    pub fuel_limit: u64,
    pub entry_point: Option<String>,
}

impl Default for WasmUdfConfig {
    fn default() -> Self {
        Self {
            memory_limit_bytes: 64 * 1024, // 64 KiB = 1 WebAssembly page
            fuel_limit: 500_000,           // 500,000 instructions per row
            entry_point: None,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WasmUdfInfo {
    pub name: String,
    pub memory_limit_bytes: usize,
    pub fuel_limit: u64,
    pub bytecode_len: usize,
    pub created_at: i64,
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug)]
struct UdfLimiter {
    memory_limit_bytes: usize,
}

#[cfg(not(target_arch = "wasm32"))]
impl wasmtime::ResourceLimiter for UdfLimiter {
    fn memory_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> std::result::Result<bool, wasmtime::Error> {
        Ok(desired <= self.memory_limit_bytes)
    }

    fn table_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> std::result::Result<bool, wasmtime::Error> {
        Ok(desired <= 100)
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug)]
pub struct UdfStoreData {
    limiter: UdfLimiter,
}

#[cfg(not(target_arch = "wasm32"))]
pub struct RegisteredUdf {
    pub name: String,
    pub config: WasmUdfConfig,
    pub bytecode: Vec<u8>,
    pub created_at: i64,
    pub module: wasmtime::Module,
}

#[cfg(not(target_arch = "wasm32"))]
impl std::fmt::Debug for RegisteredUdf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegisteredUdf")
            .field("name", &self.name)
            .field("config", &self.config)
            .field("bytecode_len", &self.bytecode.len())
            .field("created_at", &self.created_at)
            .finish()
    }
}

#[cfg(target_arch = "wasm32")]
#[derive(Debug)]
pub struct RegisteredUdf {
    pub name: String,
    pub config: WasmUdfConfig,
    pub bytecode: Vec<u8>,
    pub created_at: i64,
}

#[derive(Clone)]
pub struct WasmUdfRegistry {
    #[cfg(not(target_arch = "wasm32"))]
    engine: wasmtime::Engine,
    modules: Arc<RwLock<HashMap<String, Arc<RegisteredUdf>>>>,
}

impl std::fmt::Debug for WasmUdfRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WasmUdfRegistry")
            .field("count", &self.modules.read().map(|m| m.len()).unwrap_or(0))
            .finish()
    }
}

impl WasmUdfRegistry {
    #[must_use]
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let mut config = wasmtime::Config::new();
            config.consume_fuel(true);
            let engine = wasmtime::Engine::new(&config).expect("valid wasmtime configuration");
            Self {
                engine,
                modules: Arc::new(RwLock::new(HashMap::new())),
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            Self {
                modules: Arc::new(RwLock::new(HashMap::new())),
            }
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub fn engine(&self) -> &wasmtime::Engine {
        &self.engine
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn register(
        &self,
        name: &str,
        bytecode_or_wat: &[u8],
        config: WasmUdfConfig,
        created_at: i64,
    ) -> Result<Arc<RegisteredUdf>> {
        let module = wasmtime::Module::new(&self.engine, bytecode_or_wat).map_err(|e| {
            LiliaError::new(
                ErrorCode::InvalidInput,
                format!("failed to compile wasm module for udf '{name}': {e}"),
                false,
            )
        })?;

        let entry = config.entry_point.as_deref().unwrap_or(name);
        if module.get_export(entry).is_none() {
            return Err(LiliaError::new(
                ErrorCode::InvalidInput,
                format!("wasm module does not export entry function '{entry}' for udf '{name}'"),
                false,
            ));
        }

        let registered = Arc::new(RegisteredUdf {
            name: name.to_string(),
            config,
            bytecode: bytecode_or_wat.to_vec(),
            created_at,
            module,
        });

        let mut lock = self
            .modules
            .write()
            .map_err(|_| LiliaError::new(ErrorCode::Storage, "registry lock poisoned", false))?;
        lock.insert(name.to_string(), Arc::clone(&registered));
        Ok(registered)
    }

    #[cfg(target_arch = "wasm32")]
    pub fn register(
        &self,
        _name: &str,
        _bytecode_or_wat: &[u8],
        _config: WasmUdfConfig,
        _created_at: i64,
    ) -> Result<Arc<RegisteredUdf>> {
        Err(LiliaError::new(
            ErrorCode::InvalidInput,
            "wasm udfs are not supported on wasm32 target",
            false,
        ))
    }

    pub fn unregister(&self, name: &str) -> Option<Arc<RegisteredUdf>> {
        let mut lock = self.modules.write().ok()?;
        lock.remove(name)
    }

    pub fn get(&self, name: &str) -> Option<Arc<RegisteredUdf>> {
        let lock = self.modules.read().ok()?;
        lock.get(name).cloned()
    }

    pub fn list(&self) -> Vec<WasmUdfInfo> {
        let lock = match self.modules.read() {
            Ok(guard) => guard,
            Err(_) => return Vec::new(),
        };
        lock.values()
            .map(|udf| WasmUdfInfo {
                name: udf.name.clone(),
                memory_limit_bytes: udf.config.memory_limit_bytes,
                fuel_limit: udf.config.fuel_limit,
                bytecode_len: udf.bytecode.len(),
                created_at: udf.created_at,
            })
            .collect()
    }

    pub fn init_catalog_table(conn: &NativeConnection) -> Result<()> {
        conn.exec(
            "CREATE TABLE IF NOT EXISTS _lilia_wasm_udf (
                name TEXT PRIMARY KEY,
                bytecode BLOB NOT NULL,
                memory_limit_bytes INTEGER NOT NULL,
                fuel_limit INTEGER NOT NULL,
                entry_point TEXT,
                created_at INTEGER NOT NULL
            );",
        )?;
        Ok(())
    }

    pub fn save_to_catalog(conn: &NativeConnection, udf: &RegisteredUdf) -> Result<()> {
        Self::init_catalog_table(conn)?;
        let mut stmt = conn.prepare(
            "INSERT OR REPLACE INTO _lilia_wasm_udf 
             (name, bytecode, memory_limit_bytes, fuel_limit, entry_point, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6);",
        )?;
        stmt.bind_text_static(1, &udf.name)?;
        stmt.bind_blob_static(2, &udf.bytecode)?;
        stmt.bind_int64(3, udf.config.memory_limit_bytes as i64)?;
        stmt.bind_int64(4, udf.config.fuel_limit as i64)?;
        if let Some(ref ep) = udf.config.entry_point {
            stmt.bind_text_static(5, ep)?;
        } else {
            stmt.bind_null(5)?;
        }
        stmt.bind_int64(6, udf.created_at)?;
        stmt.step()?;
        Ok(())
    }

    pub fn delete_from_catalog(conn: &NativeConnection, name: &str) -> Result<()> {
        Self::init_catalog_table(conn)?;
        let mut stmt = conn.prepare("DELETE FROM _lilia_wasm_udf WHERE name = ?1;")?;
        stmt.bind_text_static(1, name)?;
        stmt.step()?;
        Ok(())
    }

    pub fn load_catalog(&self, conn: &NativeConnection) -> Result<usize> {
        Self::init_catalog_table(conn)?;
        let mut stmt = conn.prepare(
            "SELECT name, bytecode, memory_limit_bytes, fuel_limit, entry_point, created_at
             FROM _lilia_wasm_udf;",
        )?;

        let mut loaded = 0;
        while stmt.step()? {
            let name = stmt.column_text(0).to_string();
            let bytecode = stmt.column_blob(1).to_vec();
            let memory_limit_bytes = stmt.column_int64(2) as usize;
            let fuel_limit = stmt.column_int64(3) as u64;
            let entry_point = if stmt.column_type(4) == lilia_sqlite_native::ffi::SQLITE_NULL {
                None
            } else {
                Some(stmt.column_text(4).to_string())
            };
            let created_at = stmt.column_int64(5);

            let config = WasmUdfConfig {
                memory_limit_bytes,
                fuel_limit,
                entry_point,
            };

            if self.register(&name, &bytecode, config, created_at).is_ok() {
                loaded += 1;
            }
        }
        Ok(loaded)
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub struct WasmUdfCallContext {
    pub store: wasmtime::Store<UdfStoreData>,
    pub instance: wasmtime::Instance,
    pub func: wasmtime::Func,
    pub memory: Option<wasmtime::Memory>,
    pub fuel_limit: u64,
    pub alloc_fn: Option<wasmtime::TypedFunc<i32, i32>>,
    pub dealloc_fn: Option<wasmtime::TypedFunc<(i32, i32), ()>>,
}

#[cfg(not(target_arch = "wasm32"))]
impl std::fmt::Debug for WasmUdfCallContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WasmUdfCallContext")
            .field("fuel_limit", &self.fuel_limit)
            .finish()
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl WasmUdfCallContext {
    pub fn new(engine: &wasmtime::Engine, registered: &RegisteredUdf) -> Result<Self> {
        let limiter = UdfLimiter {
            memory_limit_bytes: registered.config.memory_limit_bytes,
        };
        let mut store = wasmtime::Store::new(engine, UdfStoreData { limiter });
        store.limiter(|s| &mut s.limiter);
        store.set_fuel(registered.config.fuel_limit).map_err(|e| {
            LiliaError::new(
                ErrorCode::Storage,
                format!("failed to set initial fuel: {e}"),
                false,
            )
        })?;

        // Zero host imports: pure computational enclave!
        let linker = wasmtime::Linker::new(engine);
        let instance = linker
            .instantiate(&mut store, &registered.module)
            .map_err(|e| {
                LiliaError::new(
                    ErrorCode::Storage,
                    format!("failed to instantiate wasm udf '{}': {e}", registered.name),
                    false,
                )
            })?;

        let entry = registered
            .config
            .entry_point
            .as_deref()
            .unwrap_or(&registered.name);
        let func = instance.get_func(&mut store, entry).ok_or_else(|| {
            LiliaError::new(
                ErrorCode::InvalidInput,
                format!("wasm udf missing export '{entry}'"),
                false,
            )
        })?;

        let memory = instance.get_memory(&mut store, "memory");
        let alloc_fn = instance
            .get_typed_func::<i32, i32>(&mut store, "lilia_alloc")
            .or_else(|_| instance.get_typed_func::<i32, i32>(&mut store, "alloc"))
            .or_else(|_| instance.get_typed_func::<i32, i32>(&mut store, "malloc"))
            .ok();

        let dealloc_fn = instance
            .get_typed_func::<(i32, i32), ()>(&mut store, "lilia_dealloc")
            .or_else(|_| instance.get_typed_func::<(i32, i32), ()>(&mut store, "dealloc"))
            .ok();

        Ok(Self {
            store,
            instance,
            func,
            memory,
            fuel_limit: registered.config.fuel_limit,
            alloc_fn,
            dealloc_fn,
        })
    }

    #[allow(clippy::too_many_lines)]
    pub fn invoke(&mut self, ctx: &NativeContext, user_args: &[NativeValue]) -> Result<()> {
        self.store
            .set_fuel(self.fuel_limit)
            .map_err(map_wasm_trap)?;

        let ty = self.func.ty(&self.store);
        let param_types: Vec<wasmtime::ValType> = ty.params().collect();
        let result_types: Vec<wasmtime::ValType> = ty.results().collect();

        // Pattern 1: Numerical scalar mapping (Direct registers, zero-allocation)
        if param_types.len() == user_args.len() && param_types.iter().all(is_numerical_type) {
            let mut wasm_params = Vec::with_capacity(param_types.len());
            for (arg, param_ty) in user_args.iter().zip(&param_types) {
                match param_ty {
                    wasmtime::ValType::F64 => {
                        wasm_params.push(wasmtime::Val::F64(arg.as_double().to_bits()));
                    }
                    wasmtime::ValType::F32 => {
                        let f = arg.as_double() as f32;
                        wasm_params.push(wasmtime::Val::F32(f.to_bits()));
                    }
                    wasmtime::ValType::I64 => {
                        wasm_params.push(wasmtime::Val::I64(arg.as_int64()));
                    }
                    wasmtime::ValType::I32 => {
                        wasm_params.push(wasmtime::Val::I32(arg.as_int64() as i32));
                    }
                    _ => unreachable!(),
                }
            }

            let mut wasm_results = vec![wasmtime::Val::I32(0); result_types.len()];
            if let Err(trap) = self
                .func
                .call(&mut self.store, &wasm_params, &mut wasm_results)
            {
                let err = map_wasm_trap(trap);
                ctx.set_result_error(&err.to_string());
                return Ok(());
            }

            set_native_result(ctx, &wasm_results);
            return Ok(());
        }

        // Pattern 2: Single String/Blob with (in_ptr: i32, in_len: i32)
        if param_types.len() == 2
            && matches!(param_types[0], wasmtime::ValType::I32)
            && matches!(param_types[1], wasmtime::ValType::I32)
            && user_args.len() == 1
        {
            let input_bytes = if user_args[0].is_blob() {
                user_args[0].as_blob()
            } else {
                user_args[0].as_text().as_bytes()
            };

            let Some(ref alloc) = self.alloc_fn else {
                ctx.set_result_error("wasm udf requires an exported memory allocator (alloc/lilia_alloc) for string/blob arguments");
                return Ok(());
            };

            let in_len = input_bytes.len() as i32;
            let in_ptr = match alloc.call(&mut self.store, in_len) {
                Ok(ptr) if ptr > 0 => ptr as u32,
                _ => {
                    ctx.set_result_error("wasm guest memory allocation failed");
                    return Ok(());
                }
            };

            let Some(ref mem) = self.memory else {
                ctx.set_result_error("wasm udf missing linear memory export");
                return Ok(());
            };

            if let Err(e) = mem.write(&mut self.store, in_ptr as usize, input_bytes) {
                ctx.set_result_error(&format!("wasm memory write error: {e}"));
                return Ok(());
            }

            let wasm_params = [
                wasmtime::Val::I32(in_ptr as i32),
                wasmtime::Val::I32(in_len),
            ];
            let mut wasm_results = vec![wasmtime::Val::I64(0); result_types.len()];

            let call_res = self
                .func
                .call(&mut self.store, &wasm_params, &mut wasm_results);

            // Clean up guest input buffer
            if let Some(ref dealloc) = self.dealloc_fn {
                let _ = dealloc.call(&mut self.store, (in_ptr as i32, in_len));
            }

            if let Err(trap) = call_res {
                let err = map_wasm_trap(trap);
                ctx.set_result_error(&err.to_string());
                return Ok(());
            }

            // Inspect return value
            if result_types.len() == 1 && matches!(result_types[0], wasmtime::ValType::I64) {
                if let wasmtime::Val::I64(packed) = wasm_results[0] {
                    let out_ptr = ((packed >> 32) & 0xFFFF_FFFF) as u32 as usize;
                    let out_len = (packed & 0xFFFF_FFFF) as u32 as usize;

                    if out_ptr > 0 && out_len > 0 {
                        let mut buf = vec![0u8; out_len];
                        if let Err(e) = mem.read(&self.store, out_ptr, &mut buf) {
                            ctx.set_result_error(&format!("wasm memory read error: {e}"));
                            return Ok(());
                        }

                        if let Some(ref dealloc) = self.dealloc_fn {
                            let _ = dealloc.call(&mut self.store, (out_ptr as i32, out_len as i32));
                        }

                        if let Ok(s) = std::str::from_utf8(&buf) {
                            ctx.set_result_text(s);
                        } else {
                            ctx.set_result_blob(&buf);
                        }
                        return Ok(());
                    }
                }
            }

            set_native_result(ctx, &wasm_results);
            return Ok(());
        }

        // Pattern 3: Buffer Output: (in_ptr: i32, in_len: i32, out_ptr: i32, out_cap: i32) -> i32 (out_len)
        if param_types.len() == 4
            && param_types
                .iter()
                .all(|t| matches!(t, wasmtime::ValType::I32))
            && user_args.len() == 1
        {
            let input_bytes = if user_args[0].is_blob() {
                user_args[0].as_blob()
            } else {
                user_args[0].as_text().as_bytes()
            };

            let Some(ref alloc) = self.alloc_fn else {
                ctx.set_result_error("wasm udf requires an exported memory allocator");
                return Ok(());
            };

            let in_len = input_bytes.len() as i32;
            let in_ptr = match alloc.call(&mut self.store, in_len) {
                Ok(ptr) if ptr > 0 => ptr as u32,
                _ => {
                    ctx.set_result_error("wasm memory alloc failed for input");
                    return Ok(());
                }
            };

            let out_cap = in_len + 256;
            let out_ptr = match alloc.call(&mut self.store, out_cap) {
                Ok(ptr) if ptr > 0 => ptr as u32,
                _ => {
                    if let Some(ref dealloc) = self.dealloc_fn {
                        let _ = dealloc.call(&mut self.store, (in_ptr as i32, in_len));
                    }
                    ctx.set_result_error("wasm memory alloc failed for output buffer");
                    return Ok(());
                }
            };

            let Some(ref mem) = self.memory else {
                ctx.set_result_error("wasm udf missing linear memory export");
                return Ok(());
            };

            if let Err(e) = mem.write(&mut self.store, in_ptr as usize, input_bytes) {
                ctx.set_result_error(&format!("wasm memory write error: {e}"));
                return Ok(());
            }

            let wasm_params = [
                wasmtime::Val::I32(in_ptr as i32),
                wasmtime::Val::I32(in_len),
                wasmtime::Val::I32(out_ptr as i32),
                wasmtime::Val::I32(out_cap),
            ];
            let mut wasm_results = [wasmtime::Val::I32(0)];

            let call_res = self
                .func
                .call(&mut self.store, &wasm_params, &mut wasm_results);

            let res = match call_res {
                Ok(()) => {
                    let out_len = match wasm_results[0] {
                        wasmtime::Val::I32(len) if len >= 0 => len as usize,
                        _ => 0,
                    };
                    if out_len > out_cap as usize {
                        ctx.set_result_error("wasm udf output length exceeded capacity");
                        Ok(())
                    } else {
                        let mut buf = vec![0u8; out_len];
                        if let Err(e) = mem.read(&self.store, out_ptr as usize, &mut buf) {
                            ctx.set_result_error(&format!("wasm memory read error: {e}"));
                            Ok(())
                        } else {
                            if let Ok(s) = std::str::from_utf8(&buf) {
                                ctx.set_result_text(s);
                            } else {
                                ctx.set_result_blob(&buf);
                            }
                            Ok(())
                        }
                    }
                }
                Err(trap) => {
                    let err = map_wasm_trap(trap);
                    ctx.set_result_error(&err.to_string());
                    Ok(())
                }
            };

            if let Some(ref dealloc) = self.dealloc_fn {
                let _ = dealloc.call(&mut self.store, (in_ptr as i32, in_len));
                let _ = dealloc.call(&mut self.store, (out_ptr as i32, out_cap));
            }

            return res;
        }

        ctx.set_result_error(&format!(
            "wasm udf signature mismatch: expected {} params, got {}",
            param_types.len(),
            user_args.len()
        ));
        Ok(())
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn is_numerical_type(ty: &wasmtime::ValType) -> bool {
    matches!(
        ty,
        wasmtime::ValType::I32
            | wasmtime::ValType::I64
            | wasmtime::ValType::F32
            | wasmtime::ValType::F64
    )
}

#[cfg(not(target_arch = "wasm32"))]
fn set_native_result(ctx: &NativeContext, results: &[wasmtime::Val]) {
    if results.is_empty() {
        ctx.set_result_null();
        return;
    }
    match results[0] {
        wasmtime::Val::F64(bits) => ctx.set_result_double(f64::from_bits(bits)),
        wasmtime::Val::F32(bits) => ctx.set_result_double(f64::from(f32::from_bits(bits))),
        wasmtime::Val::I64(v) => ctx.set_result_int64(v),
        wasmtime::Val::I32(v) => ctx.set_result_int64(i64::from(v)),
        _ => ctx.set_result_null(),
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn map_wasm_trap<E: std::fmt::Display + std::fmt::Debug>(err: E) -> LiliaError {
    let debug_msg = format!("{err:?}");
    let display_msg = format!("{err}");
    let full = format!("{display_msg} {debug_msg}").to_lowercase();
    if full.contains("fuel") {
        LiliaError::new(
            ErrorCode::Timeout,
            format!("wasm execution fuel exhausted: {display_msg}"),
            false,
        )
    } else if full.contains("out of bounds")
        || full.contains("memory")
        || full.contains("memoryoutofbounds")
    {
        LiliaError::new(
            ErrorCode::Corrupt,
            format!("wasm memory trap: {display_msg}"),
            false,
        )
    } else {
        LiliaError::new(
            ErrorCode::Storage,
            format!("wasm trap: {display_msg}"),
            false,
        )
    }
}
