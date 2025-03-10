# function-runner

This fork of the [Shopify Function Runner](https://github.com/Shopify/function-runner) is a demo of usage of the [Dylibso Observe SDK](https://github.com/dylibso/observe-sdk).

This README will show you how to instrument a Shopify Function and view the data in [Zipkin](https://zipkin.io/).

## Demo

<a href="https://www.loom.com/share/3bb8a3da8ec24d77a7c77265fba27f2d">
    <img style="max-width:300px;" src="https://cdn.loom.com/sessions/thumbnails/3bb8a3da8ec24d77a7c77265fba27f2d-with-play.gif">
</a>

## Instructions

### Install this forked version of the runner

> *Note*: This will install over an existing version of `function-runner` if you have one.

```bash
git clone https://github.com/dylibso/function-runner.git
cd function-runner
cargo install --path . --locked
```

If you don't get any error messages, check that it installed correctly. You should see a version with `-observe` in it:

```bash
function-runner --version
# => function-runner 3.5.0-observe
```

### Run Zipkin

The easiest way to run zipkin locally is with Docker:


```bash
docker run -d -p 9411:9411 openzipkin/zipkin --logging.level.zipkin2=DEBUG
```

### Run your function

Run your function like you normally would with the shopify function runner. It will first send your Wasm to our instrumenter service to be instrumented (see more info in next section), then it will run it and emit a URL which you can click to view the trace:

> *Note*: The instrumenter works on all Wasm files, but we only offer support to shopify functions written in Rust for the scope of this demo

```bash
function-runner -f example/discount.wasm -j example/discount.json
# => http://localhost:9411/zipkin/traces/41c49675061b0c99
```

### Aside: Instrumenting Wasm Module

*Note*: This demo auto-instruments the code for you with a trial API key, but this section describes how the service works

### Using your own API key

Contact support@dylibso.com and use your own API key via:
```bash
export DYLIBSO_OBSERVE_API_KEY=xxxxxxxxxxxxxxxxxxxxxxxxx
function-runner -f example/discount.wasm -j example/discount.json
```

You can now instrument your Shopify function with our instrumenter. The only way to instrument your Wasm right now is through the instrumentation service. The easiest way to do this is to send up your Wasm with curl and get an instrumented Wasm module back:

```bash
curl -F wasm=@code.wasm https://compiler-preview.dylibso.com/instrument -X POST -H 'Authorization: Bearer <your-api-key>' > code.instr.wasm
```

:key: **You can get an API key by contacting [support@dylibso.com](mailto:support@dylibso.com).**

> **Note**: The Instrumentation Service (https://compiler-preview.dylibso.com/instrument) only re-compiles a Wasm binary and returns the updated code. We do not log or store any information about your submitted code. The compilation also adds no telemetry or other information besides the strictly-necessary auto-instrumentation to the Wasm instructions. If you would prefer to run this service yourself, please contact [support@dylibso.com](mailto:support@dylibso.com) to discuss the available options.


