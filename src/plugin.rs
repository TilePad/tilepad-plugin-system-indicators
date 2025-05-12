use std::{rc::Rc, time::Duration};

use base64::{Engine, engine::general_purpose};
use lhm_client::{ComputerOptions, HardwareType, LHMClient, Sensor, SensorType};
use parking_lot::Mutex;
use tilepad_plugin_sdk::{plugin::Plugin, protocol::TileIcon, session::PluginSessionHandle};
use tokio::{task::spawn_local, time::sleep};

#[derive(Default)]
pub struct ExamplePlugin {}

impl ExamplePlugin {
    pub fn new() -> Self {
        Default::default()
    }
}

impl Plugin for ExamplePlugin {
    fn on_registered(&mut self, session: &PluginSessionHandle) {
        let session = session.clone();
        spawn_local(perform_background_updates(session));
    }
}

async fn perform_background_updates(session: PluginSessionHandle) {
    let current_tiles = Rc::new(Mutex::new(Vec::new()));

    // Background task to update the set of tiles every 5 seconds
    spawn_local({
        let session = session.clone();
        let current_tiles = current_tiles.clone();
        async move {
            // Request the current set of tiles every 5 seconds
            loop {
                if let Ok(tiles) = session.get_visible_tiles().await {
                    *current_tiles.lock() = tiles;
                }

                sleep(Duration::from_secs(5)).await;
            }
        }
    });

    let mut client = LHMClient::connect().await.unwrap();
    client
        .set_options(ComputerOptions {
            cpu_enabled: true,
            ..Default::default()
        })
        .await
        .unwrap();

    client.update().await.unwrap();

    loop {
        // Update the current hardware
        client.update().await.unwrap();

        // Request the current hardware
        let hardware = client.get_hardware().await.unwrap();

        let cpu_temps: Vec<&Sensor> = hardware
            .iter()
            // Filter the hardware for the CPU
            .filter(|value| matches!(value.ty, HardwareType::Cpu))
            // Get only the temperature sensors
            .flat_map(|value| {
                value
                    .sensors
                    .iter()
                    .filter(|value| matches!(value.ty, SensorType::Temperature))
            })
            .collect();

        let temp = cpu_temps
            .iter()
            .find(|sensor| sensor.name.eq("CPU Package"));

        let temp = temp.map(|value| value.value).expect("Unknown CPU Temp");
        let icon = cpu_temp_indicator_svg(temp);

        {
            for tile in current_tiles.lock().iter() {
                _ = session.set_tile_icon(tile.id, TileIcon::Url { src: icon.clone() });
                let mut label = tile.config.label.clone();
                label.label = Some(format!("CPU: {temp:.0}°C"));
                _ = session.set_tile_label(tile.id, label);
            }
        }

        sleep(Duration::from_secs(2)).await;
    }
}

pub fn cpu_temp_indicator_svg(temp_celsius: f32) -> String {
    // Choose color based on temperature
    let color = if temp_celsius < 50.0 {
        "#4caf50" // green
    } else if temp_celsius < 70.0 {
        "#ff9800" // orange
    } else {
        "#f44336" // red
    };

    // Convert temp to a percentage (0-100°C -> 0-100%)
    let clamped_temp = temp_celsius.clamp(0.0, 100.0);
    let percent = clamped_temp / 100.0;
    let radius = 40.0;
    let circumference = 2.0 * std::f32::consts::PI * radius;
    let dash_offset = circumference * (1.0 - percent);

    let svg = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100" viewBox="0 0 100 100">
        <defs>
            <linearGradient id="grad" x1="0%" y1="0%" x2="100%" y2="0%">
                <stop offset="0%" style="stop-color:{color};stop-opacity:1" />
                <stop offset="100%" style="stop-color:#ffffff;stop-opacity:0.2" />
            </linearGradient>
        </defs>
        <circle cx="50" cy="50" r="{radius}" stroke="#333" stroke-width="10" fill="none"/>
        <circle cx="50" cy="50" r="{radius}" stroke="url(#grad)" stroke-width="10" fill="none"
            stroke-dasharray="{circumference}" stroke-dashoffset="{dash_offset}"
            transform="rotate(-90 50 50)">
        </circle>
    </svg>"##,
        radius = radius,
        circumference = circumference,
        dash_offset = dash_offset,
        color = color,
    );

    let encoded = general_purpose::STANDARD.encode(svg);
    format!("data:image/svg+xml;base64,{}", encoded)
}
