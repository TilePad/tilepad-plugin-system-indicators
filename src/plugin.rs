use anyhow::Context;
use lhm_client::{ComputerOptions, HardwareType, LHMClient, LHMClientHandle, Sensor, SensorType};
use serde::{Deserialize, Serialize};
use std::{cell::Cell, rc::Rc, time::Duration};
use tilepad_plugin_sdk::{plugin::Plugin, session::PluginSessionHandle, tracing};
use tokio::{
    task::{JoinHandle, spawn_local},
    time::sleep,
};

#[derive(Default)]
pub struct IndicatorsPlugin {
    /// Current CPU temperature value
    cpu_value: Rc<Cell<f32>>,

    /// Handle for the task managing CPU requests
    cpu_task: Option<JoinHandle<()>>,
}

/// Message from the display
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]
enum DisplayMessageIn {
    GetCpuTemp,
}

/// Message sent to the display
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]
enum DisplayMessageOut {
    CpuTemp { value: f32 },
}

impl Plugin for IndicatorsPlugin {
    fn on_display_message(
        &mut self,
        _session: &PluginSessionHandle,
        display: tilepad_plugin_sdk::display::Display,
        message: serde_json::Value,
    ) {
        let message: DisplayMessageIn = match serde_json::from_value(message) {
            Ok(value) => value,
            Err(cause) => {
                tracing::warn!(?cause, "failed to deserialize display message");
                return;
            }
        };

        match message {
            DisplayMessageIn::GetCpuTemp => {
                // Initialize the background task on first request
                if self.cpu_task.is_none() {
                    let task = spawn_local(run_computer_monitor(self.cpu_value.clone()));
                    self.cpu_task = Some(task);
                }

                // Get the current value and send it back to the display
                let value = self.cpu_value.get();
                _ = display.send(DisplayMessageOut::CpuTemp { value });
            }
        }
    }
}

// Find a sensor for the current CPU
async fn get_cpu_sensor(client: &LHMClientHandle) -> anyhow::Result<Sensor> {
    // Query for CPU hardware
    let cpu_hardware = client.query_hardware(None, Some(HardwareType::Cpu)).await?;

    // Get the first CPU hardware
    let cpu = cpu_hardware
        .into_iter()
        .next()
        .context("missing cpu hardware")?;

    // Query the cpu hardware for temperature sensors
    let sensors = client
        .query_sensors(Some(cpu.identifier), Some(SensorType::Temperature))
        .await?;

    // Get the sensor for the CPU Package
    let sensor = sensors
        .into_iter()
        .find(|sensor| sensor.name.eq("CPU Package"))
        .context("missing cpu sensor")?;

    Ok(sensor)
}

/// Run the monitoring task that polls for the CPU temperature every second
async fn run_computer_monitor(cpu_value: Rc<Cell<f32>>) {
    let client = match LHMClient::connect().await {
        Ok(value) => value,
        Err(cause) => {
            tracing::error!(?cause, "failed to connect to monitoring service");
            return;
        }
    };

    // Set the options for what we want to request
    if let Err(cause) = client
        .set_options(ComputerOptions {
            cpu_enabled: true,
            gpu_enabled: true,
            memory_enabled: true,
            ..Default::default()
        })
        .await
    {
        tracing::error!(?cause, "failed to set monitor service options");
        return;
    };

    // Load the available hardware
    if let Err(cause) = client.update_all().await {
        tracing::error!(?cause, "failed to update monitor service");
        return;
    }

    run_cpu_sensor(&client, cpu_value).await;
}

/// Run a loop for the CPU sensor storing its current temperature value in `cpu_value`
async fn run_cpu_sensor(client: &LHMClientHandle, cpu_value: Rc<Cell<f32>>) {
    // Get the CPU sensor
    let mut cpu_sensor = match get_cpu_sensor(client).await {
        Ok(value) => value,
        Err(cause) => {
            tracing::error!(?cause, "failed to obtain cpu sensor");
            return;
        }
    };

    loop {
        // Get the current value of the CPUs sensor
        let value = match client
            .get_sensor_value_by_id(cpu_sensor.identifier.clone(), true)
            .await
        {
            Ok(Some(value)) => value,
            // CPU sensor was lost (Another client refreshed cache?)
            Ok(None) => {
                // Try and obtain the CPU sensor again
                if let Ok(value) = get_cpu_sensor(client).await {
                    cpu_sensor = value;
                    continue;
                }

                // Some other factor is preventing us from gaining the CPU sensor
                tracing::warn!("cpu temperature sensor no longer exists");
                return;
            }

            Err(cause) => {
                tracing::error!(?cause, "failed to get current temperature value");
                return;
            }
        };

        // Update the current temperature value
        cpu_value.set(value);

        // Wait till the next tick
        sleep(Duration::from_secs(1)).await;
    }
}
