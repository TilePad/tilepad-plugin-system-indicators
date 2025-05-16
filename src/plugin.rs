use std::{rc::Rc, time::Duration};

use lhm_client::{ComputerOptions, HardwareType, LHMClient, SensorType};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tilepad_plugin_sdk::{plugin::Plugin, session::PluginSessionHandle};
use tokio::{
    sync::{mpsc, oneshot},
    task::spawn_local,
    time::sleep,
};

pub struct ExamplePlugin {
    tx: mpsc::UnboundedSender<ActorMessage>,
}

impl ExamplePlugin {
    pub fn create() -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel();
        spawn_local(async move {
            let cpu_value = Rc::new(Mutex::new(0.0));

            spawn_local({
                let cpu_value = cpu_value.clone();
                async move {
                    let client = LHMClient::connect().await.unwrap();

                    client
                        .set_options(ComputerOptions {
                            cpu_enabled: true,
                            gpu_enabled: true,
                            memory_enabled: true,
                            ..Default::default()
                        })
                        .await
                        .unwrap();

                    client.update_all().await.unwrap();

                    let cpus = client
                        .query_hardware(None, Some(HardwareType::Cpu))
                        .await
                        .unwrap();

                    let cpu = cpus.first().unwrap();

                    let sensors = client
                        .query_sensors(Some(cpu.identifier.clone()), Some(SensorType::Temperature))
                        .await
                        .unwrap();

                    let sensor = sensors
                        .iter()
                        .find(|sensor| sensor.name.eq("CPU Package"))
                        .unwrap();

                    loop {
                        let value = client
                            .get_sensor_value_by_idx(sensor.index, true)
                            .await
                            .unwrap()
                            .unwrap();

                        *cpu_value.lock() = value;

                        sleep(Duration::from_secs(1)).await;
                    }
                }
            });

            while let Some(msg) = rx.recv().await {
                match msg {
                    ActorMessage::GetCpuTemp { tx } => {
                        let value = *cpu_value.lock();
                        tx.send(value);
                    }
                }
            }
        });

        Self { tx }
    }
}

enum ActorMessage {
    GetCpuTemp { tx: oneshot::Sender<f32> },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]
enum DisplayMessageIn {
    GetCpuTemp,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]
enum DisplayMessageOut {
    CpuTemp { value: f32 },
}

impl Plugin for ExamplePlugin {
    fn on_registered(&mut self, session: &PluginSessionHandle) {
        let session = session.clone();
    }

    fn on_display_message(
        &mut self,
        session: &PluginSessionHandle,
        display: tilepad_plugin_sdk::display::Display,
        message: serde_json::Value,
    ) {
        let message: DisplayMessageIn = serde_json::from_value(message).unwrap();
        match message {
            DisplayMessageIn::GetCpuTemp => {
                let (tx, rx) = oneshot::channel();

                if self.tx.send(ActorMessage::GetCpuTemp { tx }).is_err() {
                    return;
                }

                spawn_local(async move {
                    let value = rx.await.unwrap();
                    _ = display.send(DisplayMessageOut::CpuTemp { value });
                });
            }
        }
    }
}
