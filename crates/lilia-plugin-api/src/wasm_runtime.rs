use std::path::Path;
use std::sync::{Arc, Mutex};

use lilia_core::{ErrorCode, LiliaError, Result};
use wasmtime::{
    Config, Engine, Error as WasmtimeError, Instance, Linker, Memory, Module, ResourceLimiter,
    Store,
};
use wasmtime_wasi::p1::{add_to_linker_sync, WasiP1Ctx};
use wasmtime_wasi::WasiCtxBuilder;

#[derive(Debug, Clone)]
pub struct WasmPluginConfig {
    pub memory_limit_bytes: usize,
    pub fuel_limit: u64,
}

impl Default for WasmPluginConfig {
    fn default() -> Self {
        Self {
            memory_limit_bytes: 64 * 1024 * 1024, // 64 MB linear memory limit
            fuel_limit: 10_000_000,               // 10M fuel per invocation
        }
    }
}

struct PluginLimiter {
    memory_limit_bytes: usize,
}

impl ResourceLimiter for PluginLimiter {
    fn memory_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> std::result::Result<bool, WasmtimeError> {
        Ok(desired <= self.memory_limit_bytes)
    }

    fn table_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> std::result::Result<bool, WasmtimeError> {
        Ok(desired <= 10_000)
    }
}

struct WasmStoreState {
    wasi: WasiP1Ctx,
    limiter: PluginLimiter,
}

pub struct WasmInstance {
    store: Store<WasmStoreState>,
    instance: Instance,
    memory: Memory,
    config: WasmPluginConfig,
}

impl std::fmt::Debug for WasmInstance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WasmInstance").finish_non_exhaustive()
    }
}

impl WasmInstance {
    pub fn new(engine: &Engine, module: &Module, config: &WasmPluginConfig) -> Result<Self> {
        let wasi = WasiCtxBuilder::new().build_p1();
        let limiter = PluginLimiter {
            memory_limit_bytes: config.memory_limit_bytes,
        };

        let mut store = Store::new(engine, WasmStoreState { wasi, limiter });
        store.limiter(|s| &mut s.limiter);
        store.set_fuel(config.fuel_limit).map_err(map_wasm_trap)?;

        let mut linker = Linker::new(engine);
        add_to_linker_sync(&mut linker, |s: &mut WasmStoreState| &mut s.wasi)
            .map_err(map_wasm_trap)?;

        let instance = linker
            .instantiate(&mut store, module)
            .map_err(map_wasm_trap)?;

        let memory = instance.get_memory(&mut store, "memory").ok_or_else(|| {
            LiliaError::new(
                ErrorCode::Storage,
                "wasm module does not export linear memory",
                false,
            )
        })?;

        Ok(Self {
            store,
            instance,
            memory,
            config: config.clone(),
        })
    }

    pub fn alloc(&mut self, size: usize) -> Result<u32> {
        let size_i32 = i32::try_from(size).map_err(|_| {
            LiliaError::new(
                ErrorCode::InvalidInput,
                "allocation size exceeds i32",
                false,
            )
        })?;

        let alloc_fn = self
            .instance
            .get_typed_func::<i32, i32>(&mut self.store, "lilia_alloc")
            .or_else(|_| {
                self.instance
                    .get_typed_func::<i32, i32>(&mut self.store, "alloc")
            })
            .or_else(|_| {
                self.instance
                    .get_typed_func::<i32, i32>(&mut self.store, "malloc")
            })
            .map_err(|_| {
                LiliaError::new(
                    ErrorCode::Storage,
                    "wasm plugin missing memory allocator export (lilia_alloc/alloc/malloc)",
                    false,
                )
            })?;

        let ptr = alloc_fn
            .call(&mut self.store, size_i32)
            .map_err(map_wasm_trap)?;

        if ptr <= 0 {
            return Err(LiliaError::new(
                ErrorCode::Storage,
                "wasm guest allocation failed (returned null or negative pointer)",
                false,
            ));
        }

        Ok(ptr as u32)
    }

    pub fn dealloc(&mut self, ptr: u32, size: usize) {
        let ptr_i32 = ptr as i32;
        let size_i32 = size as i32;

        if let Ok(dealloc_fn) = self
            .instance
            .get_typed_func::<(i32, i32), ()>(&mut self.store, "lilia_dealloc")
            .or_else(|_| {
                self.instance
                    .get_typed_func::<(i32, i32), ()>(&mut self.store, "dealloc")
            })
        {
            let _ = dealloc_fn.call(&mut self.store, (ptr_i32, size_i32));
        } else if let Ok(free_fn) = self
            .instance
            .get_typed_func::<i32, ()>(&mut self.store, "free")
        {
            let _ = free_fn.call(&mut self.store, ptr_i32);
        }
    }

    pub fn write_bytes(&mut self, ptr: u32, bytes: &[u8]) -> Result<()> {
        self.memory
            .write(&mut self.store, ptr as usize, bytes)
            .map_err(map_wasm_trap)
    }

    pub fn read_bytes(&self, ptr: u32, len: usize) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; len];
        self.memory
            .read(&self.store, ptr as usize, &mut buf)
            .map_err(map_wasm_trap)?;
        Ok(buf)
    }

    pub fn reset_fuel(&mut self) -> Result<()> {
        self.store
            .set_fuel(self.config.fuel_limit)
            .map_err(map_wasm_trap)
    }

    pub fn encode(&mut self, plaintext: &[u8]) -> Result<Vec<u8>> {
        self.reset_fuel()?;

        let encode_fn = self
            .instance
            .get_typed_func::<(i32, i32, i32, i32, i32), i32>(&mut self.store, "codec_encode")
            .or_else(|_| {
                self.instance
                    .get_typed_func::<(i32, i32, i32, i32, i32), i32>(
                        &mut self.store,
                        "lilia_codec_encode",
                    )
            })
            .map_err(|_| {
                LiliaError::new(
                    ErrorCode::Storage,
                    "wasm plugin missing codec_encode export",
                    false,
                )
            })?;

        let in_ptr = self.alloc(plaintext.len())?;
        self.write_bytes(in_ptr, plaintext)?;

        // Allocate out buffer and out_len storage
        let out_cap = plaintext.len() + 256;
        let out_ptr = self.alloc(out_cap)?;
        let out_len_ptr = self.alloc(4)?;
        self.write_bytes(out_len_ptr, &0u32.to_le_bytes())?;

        let res = encode_fn.call(
            &mut self.store,
            (
                in_ptr as i32,
                plaintext.len() as i32,
                out_ptr as i32,
                out_cap as i32,
                out_len_ptr as i32,
            ),
        );

        let result = match res {
            Ok(0) => {
                let out_len_bytes = self.read_bytes(out_len_ptr, 4)?;
                let out_len = u32::from_le_bytes(out_len_bytes.try_into().unwrap()) as usize;
                if out_len > out_cap {
                    Err(LiliaError::new(
                        ErrorCode::Storage,
                        "wasm plugin returned out_len greater than capacity",
                        false,
                    ))
                } else {
                    self.read_bytes(out_ptr, out_len)
                }
            }
            Ok(code) => Err(LiliaError::new(
                ErrorCode::Storage,
                format!("wasm codec encode failed with code {code}"),
                false,
            )),
            Err(trap) => Err(map_wasm_trap(trap)),
        };

        self.dealloc(in_ptr, plaintext.len());
        self.dealloc(out_ptr, out_cap);
        self.dealloc(out_len_ptr, 4);

        result
    }

    pub fn decode(&mut self, ciphertext: &[u8]) -> Result<Vec<u8>> {
        self.reset_fuel()?;

        let decode_fn = self
            .instance
            .get_typed_func::<(i32, i32, i32, i32, i32), i32>(&mut self.store, "codec_decode")
            .or_else(|_| {
                self.instance
                    .get_typed_func::<(i32, i32, i32, i32, i32), i32>(
                        &mut self.store,
                        "lilia_codec_decode",
                    )
            })
            .map_err(|_| {
                LiliaError::new(
                    ErrorCode::Storage,
                    "wasm plugin missing codec_decode export",
                    false,
                )
            })?;

        let in_ptr = self.alloc(ciphertext.len())?;
        self.write_bytes(in_ptr, ciphertext)?;

        let out_cap = ciphertext.len() + 64;
        let out_ptr = self.alloc(out_cap)?;
        let out_len_ptr = self.alloc(4)?;
        self.write_bytes(out_len_ptr, &0u32.to_le_bytes())?;

        let res = decode_fn.call(
            &mut self.store,
            (
                in_ptr as i32,
                ciphertext.len() as i32,
                out_ptr as i32,
                out_cap as i32,
                out_len_ptr as i32,
            ),
        );

        let result = match res {
            Ok(0) => {
                let out_len_bytes = self.read_bytes(out_len_ptr, 4)?;
                let out_len = u32::from_le_bytes(out_len_bytes.try_into().unwrap()) as usize;
                if out_len > out_cap {
                    Err(LiliaError::new(
                        ErrorCode::Storage,
                        "wasm plugin returned out_len greater than capacity",
                        false,
                    ))
                } else {
                    self.read_bytes(out_ptr, out_len)
                }
            }
            Ok(code) => Err(LiliaError::new(
                ErrorCode::Corrupt,
                format!("wasm codec decode failed with code {code}"),
                false,
            )),
            Err(trap) => Err(map_wasm_trap(trap)),
        };

        self.dealloc(in_ptr, ciphertext.len());
        self.dealloc(out_ptr, out_cap);
        self.dealloc(out_len_ptr, 4);

        result
    }

    pub fn validate_query(
        &mut self,
        query: &str,
    ) -> std::result::Result<(), crate::PluginApiError> {
        self.reset_fuel()
            .map_err(|e| crate::PluginApiError::Invalid(e.to_string()))?;

        if let Ok(validate_fn) = self
            .instance
            .get_typed_func::<(i32, i32), i32>(&mut self.store, "sql_validate_query")
            .or_else(|_| {
                self.instance
                    .get_typed_func::<(i32, i32), i32>(&mut self.store, "lilia_sql_validate_query")
            })
        {
            let query_bytes = query.as_bytes();
            let query_ptr = self
                .alloc(query_bytes.len())
                .map_err(|e| crate::PluginApiError::Invalid(e.to_string()))?;
            self.write_bytes(query_ptr, query_bytes)
                .map_err(|e| crate::PluginApiError::Invalid(e.to_string()))?;

            let res = validate_fn.call(
                &mut self.store,
                (query_ptr as i32, query_bytes.len() as i32),
            );
            self.dealloc(query_ptr, query_bytes.len());

            match res {
                Ok(0) => Ok(()),
                Ok(code) => Err(crate::PluginApiError::Invalid(format!(
                    "SQL query validation rejected with code {code}"
                ))),
                Err(trap) => Err(crate::PluginApiError::Invalid(
                    map_wasm_trap(trap).to_string(),
                )),
            }
        } else {
            // Optional capability hook
            Ok(())
        }
    }
}

pub struct WasmInstancePool {
    engine: Engine,
    module: Module,
    config: WasmPluginConfig,
    instances: Mutex<Vec<WasmInstance>>,
}

impl std::fmt::Debug for WasmInstancePool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WasmInstancePool")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl WasmInstancePool {
    pub fn new(engine: Engine, module: Module, config: WasmPluginConfig) -> Self {
        Self {
            engine,
            module,
            config,
            instances: Mutex::new(Vec::new()),
        }
    }

    pub fn acquire(&self) -> Result<PooledWasmInstance<'_>> {
        let mut guard = self.instances.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(instance) = guard.pop() {
            return Ok(PooledWasmInstance {
                pool: self,
                instance: Some(instance),
                discard: false,
            });
        }
        drop(guard);

        // Instantiate new instance from compiled module
        let instance = WasmInstance::new(&self.engine, &self.module, &self.config)?;
        Ok(PooledWasmInstance {
            pool: self,
            instance: Some(instance),
            discard: false,
        })
    }
}

pub struct PooledWasmInstance<'a> {
    pool: &'a WasmInstancePool,
    instance: Option<WasmInstance>,
    discard: bool,
}

impl<'a> PooledWasmInstance<'a> {
    pub fn instance(&mut self) -> &mut WasmInstance {
        self.instance.as_mut().expect("instance present")
    }

    pub fn mark_discard(&mut self) {
        self.discard = true;
    }

    pub fn encode(&mut self, plaintext: &[u8]) -> Result<Vec<u8>> {
        let res = self.instance().encode(plaintext);
        if res.is_err() {
            self.discard = true;
        }
        res
    }

    pub fn decode(&mut self, ciphertext: &[u8]) -> Result<Vec<u8>> {
        let res = self.instance().decode(ciphertext);
        if res.is_err() {
            self.discard = true;
        }
        res
    }

    pub fn validate_query(
        &mut self,
        query: &str,
    ) -> std::result::Result<(), crate::PluginApiError> {
        let res = self.instance().validate_query(query);
        if res.is_err() {
            self.discard = true;
        }
        res
    }
}

impl Drop for PooledWasmInstance<'_> {
    fn drop(&mut self) {
        if let Some(instance) = self.instance.take() {
            if !self.discard {
                let mut guard = self
                    .pool
                    .instances
                    .lock()
                    .unwrap_or_else(|p| p.into_inner());
                if guard.len() < 16 {
                    guard.push(instance);
                }
            }
        }
    }
}

pub struct WasmPluginEngine {
    engine: Engine,
    module: Module,
    config: WasmPluginConfig,
}

impl WasmPluginEngine {
    pub fn load(wasm_path: &Path, config: WasmPluginConfig) -> Result<Self> {
        let wasm_bytes = std::fs::read(wasm_path).map_err(|e| {
            LiliaError::new(
                ErrorCode::Io,
                format!("failed to read wasm plugin: {e}"),
                false,
            )
        })?;
        Self::load_bytes(&wasm_bytes, config)
    }

    pub fn load_bytes(wasm_bytes: &[u8], config: WasmPluginConfig) -> Result<Self> {
        let mut wasm_config = Config::new();
        wasm_config.consume_fuel(true);

        let engine = Engine::new(&wasm_config).map_err(map_wasm_trap)?;
        let module = Module::new(&engine, wasm_bytes).map_err(map_wasm_trap)?;

        Ok(Self {
            engine,
            module,
            config,
        })
    }

    pub fn has_export(&self, name: &str) -> bool {
        self.module.get_export(name).is_some()
    }

    pub fn module(&self) -> &Module {
        &self.module
    }

    pub fn into_pool(self) -> Arc<WasmInstancePool> {
        Arc::new(WasmInstancePool::new(self.engine, self.module, self.config))
    }
}

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
