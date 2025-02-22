//! Evaluate an exported Wasm function using Wasmtime.

use crate::generators::{self, DiffValue, DiffValueType};
use crate::oracles::dummy;
use crate::oracles::engine::DiffInstance;
use crate::oracles::{compile_module, engine::DiffEngine, StoreLimits};
use anyhow::{Context, Error, Result};
use wasmtime::{Extern, FuncType, Instance, Module, Store, Trap, Val};

/// A wrapper for using Wasmtime as a [`DiffEngine`].
pub struct WasmtimeEngine {
    pub(crate) config: generators::Config,
}

impl WasmtimeEngine {
    /// Merely store the configuration; the engine is actually constructed
    /// later. Ideally the store and engine could be built here but
    /// `compile_module` takes a [`generators::Config`]; TODO re-factor this if
    /// that ever changes.
    pub fn new(config: generators::Config) -> Result<Self> {
        Ok(Self { config })
    }
}

impl DiffEngine for WasmtimeEngine {
    fn name(&self) -> &'static str {
        "wasmtime"
    }

    fn instantiate(&mut self, wasm: &[u8]) -> Result<Box<dyn DiffInstance>> {
        let store = self.config.to_store();
        let module = compile_module(store.engine(), wasm, true, &self.config).unwrap();
        let instance = WasmtimeInstance::new(store, module)?;
        Ok(Box::new(instance))
    }

    fn assert_error_match(&self, trap: &Trap, err: Error) {
        let trap2 = err.downcast::<Trap>().unwrap();
        assert_eq!(
            trap.trap_code(),
            trap2.trap_code(),
            "{}\nis not equal to\n{}",
            trap,
            trap2
        );
    }
}

/// A wrapper around a Wasmtime instance.
///
/// The Wasmtime engine constructs a new store and compiles an instance of a
/// Wasm module.
pub struct WasmtimeInstance {
    store: Store<StoreLimits>,
    instance: Instance,
}

impl WasmtimeInstance {
    /// Instantiate a new Wasmtime instance.
    pub fn new(mut store: Store<StoreLimits>, module: Module) -> Result<Self> {
        let instance = dummy::dummy_linker(&mut store, &module)
            .and_then(|l| l.instantiate(&mut store, &module))
            .context("unable to instantiate module in wasmtime")?;
        Ok(Self { store, instance })
    }

    /// Retrieve the names and types of all exported functions in the instance.
    ///
    /// This is useful for evaluating each exported function with different
    /// values. The [`DiffInstance`] trait asks for the function name and we
    /// need to know the function signature in order to pass in the right
    /// arguments.
    pub fn exported_functions(&mut self) -> Vec<(String, FuncType)> {
        let exported_functions = self
            .instance
            .exports(&mut self.store)
            .map(|e| (e.name().to_owned(), e.into_func()))
            .filter_map(|(n, f)| f.map(|f| (n, f)))
            .collect::<Vec<_>>();
        exported_functions
            .into_iter()
            .map(|(n, f)| (n, f.ty(&self.store)))
            .collect()
    }

    /// Returns the list of globals and their types exported from this instance.
    pub fn exported_globals(&mut self) -> Vec<(String, DiffValueType)> {
        let globals = self
            .instance
            .exports(&mut self.store)
            .filter_map(|e| {
                let name = e.name();
                e.into_global().map(|g| (name.to_string(), g))
            })
            .collect::<Vec<_>>();

        globals
            .into_iter()
            .map(|(name, global)| {
                (
                    name,
                    global.ty(&self.store).content().clone().try_into().unwrap(),
                )
            })
            .collect()
    }

    /// Returns the list of exported memories and whether or not it's a shared
    /// memory.
    pub fn exported_memories(&mut self) -> Vec<(String, bool)> {
        self.instance
            .exports(&mut self.store)
            .filter_map(|e| {
                let name = e.name();
                match e.into_extern() {
                    Extern::Memory(_) => Some((name.to_string(), false)),
                    Extern::SharedMemory(_) => Some((name.to_string(), true)),
                    _ => None,
                }
            })
            .collect()
    }
}

impl DiffInstance for WasmtimeInstance {
    fn name(&self) -> &'static str {
        "wasmtime"
    }

    fn evaluate(
        &mut self,
        function_name: &str,
        arguments: &[DiffValue],
        _results: &[DiffValueType],
    ) -> Result<Option<Vec<DiffValue>>> {
        let arguments: Vec<_> = arguments.iter().map(Val::from).collect();

        let function = self
            .instance
            .get_func(&mut self.store, function_name)
            .expect("unable to access exported function");
        let ty = function.ty(&self.store);
        let mut results = vec![Val::I32(0); ty.results().len()];
        function.call(&mut self.store, &arguments, &mut results)?;

        let results = results.into_iter().map(Val::into).collect();
        Ok(Some(results))
    }

    fn get_global(&mut self, name: &str, _ty: DiffValueType) -> Option<DiffValue> {
        Some(
            self.instance
                .get_global(&mut self.store, name)
                .unwrap()
                .get(&mut self.store)
                .into(),
        )
    }

    fn get_memory(&mut self, name: &str, shared: bool) -> Option<Vec<u8>> {
        Some(if shared {
            let data = self
                .instance
                .get_shared_memory(&mut self.store, name)
                .unwrap()
                .data();
            unsafe { (*data).to_vec() }
        } else {
            self.instance
                .get_memory(&mut self.store, name)
                .unwrap()
                .data(&self.store)
                .to_vec()
        })
    }
}

impl From<&DiffValue> for Val {
    fn from(v: &DiffValue) -> Self {
        match *v {
            DiffValue::I32(n) => Val::I32(n),
            DiffValue::I64(n) => Val::I64(n),
            DiffValue::F32(n) => Val::F32(n),
            DiffValue::F64(n) => Val::F64(n),
            DiffValue::V128(n) => Val::V128(n),
            DiffValue::FuncRef { null } => {
                assert!(null);
                Val::FuncRef(None)
            }
            DiffValue::ExternRef { null } => {
                assert!(null);
                Val::ExternRef(None)
            }
        }
    }
}

impl Into<DiffValue> for Val {
    fn into(self) -> DiffValue {
        match self {
            Val::I32(n) => DiffValue::I32(n),
            Val::I64(n) => DiffValue::I64(n),
            Val::F32(n) => DiffValue::F32(n),
            Val::F64(n) => DiffValue::F64(n),
            Val::V128(n) => DiffValue::V128(n),
            Val::FuncRef(f) => DiffValue::FuncRef { null: f.is_none() },
            Val::ExternRef(e) => DiffValue::ExternRef { null: e.is_none() },
        }
    }
}

#[test]
fn smoke() {
    crate::oracles::engine::smoke_test_engine(|config| WasmtimeEngine::new(config))
}
