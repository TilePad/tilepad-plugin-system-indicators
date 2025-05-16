use plugin::ExamplePlugin;
use tilepad_plugin_sdk::{setup_tracing, start_plugin};
use tokio::task::LocalSet;

pub mod plugin;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    setup_tracing();

    let local_set = LocalSet::new();

    local_set
        .run_until(async move {
            let plugin = ExamplePlugin::create();
            start_plugin(plugin).await;
        })
        .await;
}
