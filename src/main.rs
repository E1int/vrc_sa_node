use anyhow::Result;
use async_trait::async_trait;
use btleplug::api::{Central, Manager as _, Peripheral as _, ScanFilter};
use btleplug::platform::{Adapter, Manager, Peripheral};
use chrono::prelude::Local;
use clap::Parser;
use csv::Writer;
use dialoguer::{theme::ColorfulTheme, Select};
use futures::future::join_all;
use futures::StreamExt;
use rosc::{encoder, OscMessage, OscPacket, OscType};
use std::error::Error;
use std::net::UdpSocket;
use std::time::Duration;
use tokio::time;
use tracing::info;
use uuid::{uuid, Uuid};

const BATTERY_LEVEL_CHARACTERISTIC_UUID: Uuid = uuid!("00002a19-0000-1000-8000-00805f9b34fb");
const HEART_RATE_CHARACTERISTIC_UUID: Uuid = uuid!("00002a37-0000-1000-8000-00805f9b34fb");

#[async_trait]
trait AdapterExt {
    async fn scan_for(&self, seconds: u64) -> Result<()>;
}

#[async_trait]
impl AdapterExt for Adapter {
    async fn scan_for(&self, seconds: u64) -> Result<()> {
        let filter = ScanFilter::default();
        let duration = Duration::from_secs(seconds);

        self.start_scan(filter).await?;
        time::sleep(duration).await;
        self.stop_scan().await?;

        Ok(())
    }
}

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Arguments {
    /// Host address
    #[arg(long, default_value_t = String::from("127.0.0.1:9001"))]
    host: String,

    /// Client address
    #[arg(short, long, default_value_t = String::from("127.0.0.1:9000"))]
    client: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    tracing_subscriber::fmt::init();

    let arguments = Arguments::parse();
    let socket = UdpSocket::bind(&arguments.host).unwrap();
    info!("Binded to address {}", arguments.host);

    let manager = Manager::new().await?;
    let adapters = manager.adapters().await?;
    let adapter = if adapters.len() == 1 {
        adapters.first().unwrap()
    } else {
        let adpater_selection_items = join_all(
            adapters
                .iter()
                .map(|adapter| async { format!("{:?}", adapter.adapter_info().await.unwrap()) })
                .collect::<Vec<_>>(),
        )
        .await;
        let adapter_selection = Select::with_theme(&ColorfulTheme::default())
            .with_prompt("Select bluetooth adapter")
            .default(0)
            .items(&adpater_selection_items)
            .interact()
            .unwrap();
        adapters.get(adapter_selection).unwrap()
    };

    let peripheral = loop {
        adapter.scan_for(1).await?;

        let peripherals = adapter.peripherals().await?;
        if peripherals.is_empty() {
            info!("No peripherals found, scanning again");
            continue;
        }

        let mut peripheral_selection_items = vec![String::from("[Scan again]")];
        peripheral_selection_items.append(
            &mut join_all(
                peripherals
                    .iter()
                    .map(|peripheral| async {
                        format!(
                            "{:?}",
                            peripheral
                                .properties()
                                .await
                                .unwrap()
                                .unwrap()
                                .local_name
                                .unwrap()
                        )
                    })
                    .collect::<Vec<_>>(),
            )
            .await,
        );

        let peripheral_selection = Select::with_theme(&ColorfulTheme::default())
            .with_prompt("Select bluetooth peripheral")
            .default(0)
            .items(&peripheral_selection_items)
            .interact()
            .unwrap();
        if peripheral_selection == 0 {
            info!("User chose to scan again");
            continue;
        }

        break peripherals.get(peripheral_selection - 1).cloned().unwrap();
    };

    let peripheral_local_name = peripheral.properties().await?.unwrap().local_name.unwrap();
    info!("Connecting to {}", peripheral_local_name);
    peripheral.connect().await?;
    info!("Connected to {}", peripheral_local_name);

    peripheral.discover_services().await?;
    let characteristics = peripheral.characteristics();

    let battery_level_characteristic = characteristics
        .iter()
        .find(|characteristic| characteristic.uuid == BATTERY_LEVEL_CHARACTERISTIC_UUID)
        .unwrap();
    let battery_level = peripheral.read(battery_level_characteristic).await?[0];
    info!(
        "Battery level of {}: {}",
        peripheral_local_name, battery_level
    );

    let heart_rate_characteristic = characteristics
        .iter()
        .find(|characteristic| characteristic.uuid == HEART_RATE_CHARACTERISTIC_UUID)
        .unwrap();

    let log_name = format!("{}.csv", Local::now().timestamp());
    let mut writer = Writer::from_path(log_name)?;

    peripheral.subscribe(heart_rate_characteristic).await?;
    let mut notification_stream = peripheral.notifications().await?;
    while let Some(data) = notification_stream.next().await {
        info!(
            "Received data from {} [{:?}]: {:?}",
            peripheral_local_name, data.uuid, data.value
        );
        let beats_per_minute: u8 = data.value[1];
        let percent = f32::from(beats_per_minute) / f32::from(u8::MAX);
        let message = OscPacket::Message(OscMessage {
            addr: String::from("/avatar/parameters/HeartRate"),
            args: vec![OscType::Float(percent)],
        });
        let buffer = encoder::encode(&message).unwrap();
        socket.send_to(&buffer, &arguments.client).unwrap();
        info!(
            "Sent message to client [{:?}]: {:?}",
            arguments.client, message
        );

        let now = Local::now().to_rfc3339();
        let heart_rate = beats_per_minute.to_string();
        writer.write_record(&[&now, &heart_rate])?;
        writer.flush()?;
    }

    Ok(())
}
