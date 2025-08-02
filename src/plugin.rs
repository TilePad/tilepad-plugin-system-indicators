use anyhow::Context;
use lhm_client::{ComputerOptions, HardwareType, LHMClient, LHMClientHandle, Sensor, SensorType};
use serde::{Deserialize, Serialize};
use std::{cell::Cell, rc::Rc, time::Duration};
use tilepad_plugin_sdk::{Plugin, PluginSessionHandle, tracing};
use tokio::{
    sync::Mutex,
    task::{JoinHandle, spawn_local},
    time::sleep,
    try_join,
};

#[derive(Default)]
pub struct IndicatorsPlugin {
    client_handle: Rc<ManagedClient>,

    /// Current CPU temperature value
    cpu_value: Rc<Cell<f32>>,
    /// Handle for the task managing CPU requests
    cpu_task: Option<JoinHandle<()>>,

    /// Current GPU temperature value
    gpu_value: Rc<Cell<f32>>,
    /// Handle for the task managing GPU requests
    gpu_task: Option<JoinHandle<()>>,
}

/// Message from the display
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]
enum DisplayMessageIn {
    GetCpuTemp { nonce: u32 },
    GetGpuTemp { nonce: u32 },
}

/// Message sent to the display
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]
enum DisplayMessageOut {
    CpuTemp { value: f32, nonce: u32 },
    GpuTemp { value: f32, nonce: u32 },
}

impl Plugin for IndicatorsPlugin {
    fn on_display_message(
        &mut self,
        _session: &PluginSessionHandle,
        display: tilepad_plugin_sdk::Display,
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
            DisplayMessageIn::GetCpuTemp { nonce } => {
                // No client handle is initialized

                // Initialize the background task on first request
                if self.cpu_task.is_none() {
                    let task = spawn_local(run_cpu_sensor(
                        self.client_handle.clone(),
                        self.cpu_value.clone(),
                    ));
                    self.cpu_task = Some(task);
                }

                // Get the current value and send it back to the display
                let value = self.cpu_value.get();
                _ = display.send(DisplayMessageOut::CpuTemp { value, nonce });
            }
            DisplayMessageIn::GetGpuTemp { nonce } => {
                // No client handle is initialized

                // Initialize the background task on first request
                if self.gpu_task.is_none() {
                    let task = spawn_local(run_gpu_sensor(
                        self.client_handle.clone(),
                        self.gpu_value.clone(),
                    ));
                    self.gpu_task = Some(task);
                }

                // Get the current value and send it back to the display
                let value = self.gpu_value.get();
                _ = display.send(DisplayMessageOut::GpuTemp { value, nonce });
            }
        }
    }
}

#[derive(Default)]
struct ManagedClient {
    client: Mutex<Option<LHMClientHandle>>,
}

impl ManagedClient {
    pub async fn acquire(&self) -> Option<LHMClientHandle> {
        let client_lock = &mut *self.client.lock().await;

        if let Some(client) = client_lock {
            if client.is_closed() {
                // Client is closed and unavailable
                *client_lock = None;
            } else {
                // Client is available
                return Some(client.clone());
            }
        }

        let client = match LHMClient::connect().await {
            Ok(value) => value,
            Err(cause) => {
                tracing::error!(?cause, "failed to connect to monitoring service");
                return None;
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
            return None;
        };

        // Load the available hardware
        if let Err(cause) = client.update_all().await {
            tracing::error!(?cause, "failed to update monitor service");
            return None;
        }

        *client_lock = Some(client.clone());
        Some(client)
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

// Find a sensor for the current CPU
async fn get_gpu_sensor(client: &LHMClientHandle) -> anyhow::Result<Sensor> {
    // Query for GPU hardware
    let (gpu_nvidia, gpu_amd, gpu_intel) = try_join!(
        client.query_hardware(None, Some(HardwareType::GpuNvidia)),
        client.query_hardware(None, Some(HardwareType::GpuAmd)),
        client.query_hardware(None, Some(HardwareType::GpuIntel)),
    )?;

    // Get the first GPU hardware
    let gpu = gpu_nvidia
        .into_iter()
        .chain(gpu_amd.into_iter())
        .chain(gpu_intel.into_iter())
        .next()
        .context("missing gpu")?;

    // Query the cpu hardware for temperature sensors
    let sensors = client
        .query_sensors(Some(gpu.identifier), Some(SensorType::Temperature))
        .await?;

    // Get the sensor for the CPU Package
    let sensor = sensors
        .into_iter()
        .find(|sensor| sensor.name.eq("GPU Core"))
        .context("missing gpu sensor")?;

    Ok(sensor)
}

/// Run a loop for the CPU sensor storing its current temperature value in `cpu_value`
async fn run_cpu_sensor(client: Rc<ManagedClient>, cpu_value: Rc<Cell<f32>>) {
    let mut retry_attempt = 0;

    loop {
        let client = match client.acquire().await {
            Some(value) => value,
            None => {
                if retry_attempt > 3 {
                    return;
                }

                retry_attempt += 1;
                // Wait before retrying
                sleep(Duration::from_secs(5)).await;
                continue;
            }
        };

        retry_attempt = 0;

        // Get the CPU sensor
        let mut cpu_sensor = match get_cpu_sensor(&client).await {
            Ok(value) => value,
            Err(cause) => {
                tracing::error!(?cause, "failed to obtain cpu sensor");
                return;
            }
        };

        'client: loop {
            // Get the current value of the CPUs sensor
            let value = match client
                .get_sensor_value_by_id(cpu_sensor.identifier.clone(), true)
                .await
            {
                Ok(Some(value)) => value,
                // CPU sensor was lost (Another client refreshed cache?)
                Ok(None) => {
                    // Try and obtain the CPU sensor again
                    if let Ok(value) = get_cpu_sensor(&client).await {
                        cpu_sensor = value;
                        continue;
                    }

                    // Some other factor is preventing us from gaining the CPU sensor
                    tracing::warn!("cpu temperature sensor no longer exists");
                    return;
                }

                Err(cause) => {
                    tracing::error!(?cause, "failed to get current temperature value");
                    break 'client;
                }
            };

            // Update the current temperature value
            cpu_value.set(value);

            // Wait till the next tick
            sleep(Duration::from_secs(1)).await;
        }
    }
}

/// Run a loop for the GPU sensor storing its current temperature value in `gpu_value`
async fn run_gpu_sensor(client: Rc<ManagedClient>, gpu_value: Rc<Cell<f32>>) {
    let mut retry_attempt = 0;

    loop {
        let client = match client.acquire().await {
            Some(value) => value,
            None => {
                if retry_attempt > 3 {
                    return;
                }

                retry_attempt += 1;
                // Wait before retrying
                sleep(Duration::from_secs(5)).await;
                continue;
            }
        };

        retry_attempt = 0;

        // Get the GPU sensor
        let mut gpu_sensor = match get_gpu_sensor(&client).await {
            Ok(value) => value,
            Err(cause) => {
                tracing::error!(?cause, "failed to obtain cpu sensor");
                return;
            }
        };

        'client: loop {
            // Get the current value of the CPUs sensor
            let value = match client
                .get_sensor_value_by_id(gpu_sensor.identifier.clone(), true)
                .await
            {
                Ok(Some(value)) => value,
                // CPU sensor was lost (Another client refreshed cache?)
                Ok(None) => {
                    // Try and obtain the CPU sensor again
                    if let Ok(value) = get_gpu_sensor(&client).await {
                        gpu_sensor = value;
                        continue;
                    }

                    // Some other factor is preventing us from gaining the CPU sensor
                    tracing::warn!("cpu temperature sensor no longer exists");
                    return;
                }

                Err(cause) => {
                    tracing::error!(?cause, "failed to get current temperature value");
                    break 'client;
                }
            };

            // Update the current temperature value
            gpu_value.set(value);

            // Wait till the next tick
            sleep(Duration::from_secs(1)).await;
        }
    }
}
