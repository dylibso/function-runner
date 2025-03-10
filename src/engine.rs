use anyhow::{anyhow, Result};
use dylibso_observe_sdk::{adapter::zipkin::ZipkinAdapter, new_trace_id};
use rust_embed::RustEmbed;
use std::{collections::HashSet, io::Cursor, path::PathBuf};
use ureq;
use ureq_multipart::MultipartBuilder;
use wasi_common::{I32Exit, WasiCtx};
use wasmtime::{Config, Engine, Linker, Module, Store};

use crate::{
    function_run_result::{
        FunctionOutput::{self, InvalidJsonOutput, JsonOutput},
        FunctionRunResult, InvalidOutput,
    },
    logs::LogStream,
};

#[derive(RustEmbed)]
#[folder = "providers/"]
struct StandardProviders;

fn import_modules(
    module: &Module,
    engine: Engine,
    linker: &mut Linker<WasiCtx>,
    mut store: &mut Store<WasiCtx>,
) {
    let imported_modules: HashSet<String> =
        module.imports().map(|i| i.module().to_string()).collect();
    imported_modules.iter().for_each(|imported_module| {
        let imported_module_bytes = StandardProviders::get(&format!("{imported_module}.wasm"));

        if let Some(bytes) = imported_module_bytes {
            let imported_module = Module::from_binary(&engine, &bytes.data)
                .unwrap_or_else(|_| panic!("Failed to load module {imported_module}"));

            let imported_module_instance = linker
                .instantiate(&mut store, &imported_module)
                .expect("Failed to instantiate imported instance");
            linker
                .instance(
                    &mut store,
                    "javy_quickjs_provider_v1",
                    imported_module_instance,
                )
                .expect("Failed to import module");
        }
    });
}

pub async fn run(function_path: PathBuf, input: Vec<u8>) -> Result<FunctionRunResult> {
    let (content_type, data) = MultipartBuilder::new()
        .add_file("wasm", &function_path)?
        .finish()?;

    let token = std::env::var("DYLIBSO_OBSERVE_API_KEY")
        .unwrap_or_else(|_| {
            println!("The wasm code instrumentation is currently in preview, and the API key used in this demo will expire on Sept. 1 2023. Contact support@dylibso.com for your own key.");
            return "d73f9d5d88e9c0b84f8eb849458c443f".to_string();
        });
    println!("Instrumenting the module first...");
    let resp = ureq::post("https://compiler-preview.dylibso.com/instrument")
        // this key is a public, limited trial API key for this demo. please reach out to us for
        // your own key
        .set("Authorization", &format!("Bearer {}", token))
        .set("Content-Type", &content_type)
        .send_bytes(&data)?;

    let mut data = Vec::new();
    resp.into_reader().read_to_end(&mut data).unwrap();
    println!("Done!");

    let engine = Engine::new(Config::new().wasm_multi_memory(true).consume_fuel(true))?;
    let module = Module::new(&engine, &data)
        .map_err(|e| anyhow!("Couldn't load the Function {:?}: {}", &function_path, e))?;

    let input_stream = wasi_common::pipe::ReadPipe::new(Cursor::new(input));
    let output_stream = wasi_common::pipe::WritePipe::new_in_memory();
    let error_stream = wasi_common::pipe::WritePipe::new(LogStream::default());

    let memory_usage: u64;
    let instructions: u64;
    let mut error_logs: String = String::new();

    {
        let mut linker = Linker::new(&engine);
        wasmtime_wasi::add_to_linker(&mut linker, |s| s)?;
        let wasi = deterministic_wasi_ctx::build_wasi_ctx();
        wasi.set_stdin(Box::new(input_stream));
        wasi.set_stdout(Box::new(output_stream.clone()));
        wasi.set_stderr(Box::new(error_stream.clone()));
        let mut store = Store::new(&engine, wasi);
        store.add_fuel(u64::MAX)?;

        import_modules(&module, engine, &mut linker, &mut store);

        // create our adapter
        let adapter = ZipkinAdapter::create();
        let trace_ctx = adapter.start(&mut linker, &data)?;

        // optionally create/set a trace id, or the trace_ctx will assign one to this trace
        let trace_id = new_trace_id();
        trace_ctx.set_trace_id(trace_id.clone()).await;

        linker.module(&mut store, "Function", &module)?;
        let instance = linker.instantiate(&mut store, &module)?;

        let function_name = "_start";
        let function = instance.get_typed_func::<(), ()>(&mut store, function_name)?;

        let module_result = function.call(&mut store, ());

        // let module_result = instance
        //     .get_typed_func::<(), ()>(&mut store, "_start")?
        //     .call(&mut store, ());

        // modules may exit with a specific exit code, an exit code of 0 is considered success but is reported as
        // a GuestFault by wasmtime, so we need to map it to a success result. Any other exit code is considered
        // a failure.
        let module_result =
            module_result.or_else(|error| match error.downcast_ref::<wasi_common::I32Exit>() {
                Some(I32Exit(0)) => Ok(()),
                Some(I32Exit(code)) => Err(anyhow!("module exited with code: {}", code)),
                None => Err(error),
            });

        // collect the events and shut it down
        trace_ctx.shutdown().await;

        println!(
            "http://localhost:9411/zipkin/traces/{}",
            trace_id.to_hex_16()
        );

        // This is a hack to get the memory usage. Wasmtime requires a mutable borrow to a store for caching.
        // We need this mutable borrow to fall out of scope so that we can measure memory usage.
        // https://docs.rs/wasmtime/0.37.0/wasmtime/struct.Instance.html#why-does-get_export-take-a-mutable-context
        let memory_names: Vec<String> = instance
            .exports(&mut store)
            .filter(|export| export.clone().into_memory().is_some())
            .map(|export| export.name().to_string())
            .collect();

        memory_usage = memory_names
            .iter()
            .map(|name| {
                let memory = instance.get_memory(&mut store, name).unwrap();
                memory.data_size(&store) as u64
            })
            .sum::<u64>()
            / 1024;
        instructions = store.fuel_consumed().unwrap_or_default();

        match module_result {
            Ok(_) => {}
            Err(e) => {
                error_logs = e.to_string();
            }
        }
    };

    let mut logs = error_stream
        .try_into_inner()
        .expect("Log stream reference still exists");

    logs.append(error_logs.as_bytes())
        .expect("Couldn't append error logs");

    let raw_output = output_stream
        .try_into_inner()
        .expect("Output stream reference still exists")
        .into_inner();

    let output: FunctionOutput = match serde_json::from_slice(&raw_output) {
        Ok(json_output) => JsonOutput(json_output),
        Err(error) => InvalidJsonOutput(InvalidOutput {
            stdout: std::str::from_utf8(&raw_output)
                .map_err(|e| anyhow!("Couldn't print Function Output: {}", e))
                .unwrap()
                .to_owned(),
            error: error.to_string(),
        }),
    };

    let name = function_path.file_name().unwrap().to_str().unwrap();
    let size = function_path.metadata()?.len() / 1024;

    let function_run_result = FunctionRunResult::new(
        name.to_string(),
        size,
        memory_usage,
        instructions,
        logs.to_string(),
        output,
    );

    Ok(function_run_result)
}

// #[cfg(test)]
// mod tests {
//     use colored::Colorize;

//     use super::*;
//     use std::path::Path;

//     const LINEAR_MEMORY_USAGE: u64 = 159 * 64;

//     #[test]
//     fn test_js_function() {
//         let input = include_bytes!("../benchmark/build/js_function_input.json").to_vec();
//         let function_run_result = run(
//             Path::new("benchmark/build/js_function.wasm").to_path_buf(),
//             input,
//         );

//         assert!(function_run_result.is_ok());
//     }

//     #[test]
//     fn test_exit_code_zero() {
//         let input = include_bytes!("../benchmark/build/product_discount.json").to_vec();
//         let function_run_result = run(
//             Path::new("benchmark/build/exit_code_function_zero.wasm").to_path_buf(),
//             input,
//         )
//         .unwrap();

//         assert_eq!(function_run_result.logs, "");
//     }

//     #[test]
//     fn test_exit_code_one() {
//         let input = include_bytes!("../benchmark/build/product_discount.json").to_vec();
//         let function_run_result = run(
//             Path::new("benchmark/build/exit_code_function_one.wasm").to_path_buf(),
//             input,
//         )
//         .unwrap();

//         assert_eq!(function_run_result.logs, "module exited with code: 1");
//     }

//     #[test]
//     fn test_linear_memory_usage_in_kb() {
//         let input = include_bytes!("../benchmark/build/product_discount.json").to_vec();
//         let function_run_result = run(
//             Path::new("benchmark/build/linear_memory_function.wasm").to_path_buf(),
//             input,
//         )
//         .unwrap();

//         assert_eq!(function_run_result.memory_usage, LINEAR_MEMORY_USAGE);
//     }

//     #[test]
//     fn test_logs_truncation() {
//         let input = "{}".as_bytes().to_vec();
//         let function_run_result = run(
//             Path::new("benchmark/build/log_truncation_function.wasm").to_path_buf(),
//             input,
//         )
//         .unwrap();

//         assert!(function_run_result
//             .logs
//             .contains(&"...[TRUNCATED]".red().to_string()));
//     }

//     #[test]
//     fn test_file_size_in_kb() {
//         let input = include_bytes!("../benchmark/build/product_discount.json").to_vec();
//         let function_run_result = run(
//             Path::new("benchmark/build/size_function.wasm").to_path_buf(),
//             input,
//         )
//         .unwrap();

//         assert_eq!(
//             function_run_result.size,
//             Path::new("benchmark/build/size_function.wasm")
//                 .metadata()
//                 .unwrap()
//                 .len()
//                 / 1024
//         );
//     }
// }
