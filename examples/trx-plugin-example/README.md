# trx-plugin-example

This is a minimal shared-library plugin that registers a backend and frontend.
The backend is a stub that returns an error; the frontend is a no-op spawner.

Build:

```bash
cargo build -p trx-plugin-example --release
```

Install (example):

```bash
mkdir -p plugins
cp target/release/libtrx_plugin_example.* plugins/
```

Run `trx-bin` with `TRX_PLUGIN_DIRS=./plugins` to discover the plugin.
