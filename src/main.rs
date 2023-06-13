use anyhow::Result;
use async_trait::async_trait;
use btleplug::api::{BDAddr, Central, Manager as _, Peripheral as _, ScanFilter};
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
    async fn scan_for_peripheral(&self, address: BDAddr) -> Result<Peripheral>;
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

    async fn scan_for_peripheral(&self, address: BDAddr) -> Result<Peripheral> {
        info!("Scanning for peripheral with address {}", address);

        let filter = ScanFilter::default();
        let duration = Duration::from_secs(1);

        self.start_scan(filter).await?;
        let peripheral = loop {
            time::sleep(duration).await;
            let peripherals = self.peripherals().await?;
            let maybe_peripheral = peripherals
                .iter()
                .find(|peripheral| peripheral.address() == address);
            match maybe_peripheral {
                Some(peripheral) => break peripheral.clone(),
                None => continue,
            }
        };
        self.stop_scan().await?;

        info!("Peripheral with address {} found", address);

        Ok(peripheral)
    }
}

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Arguments {
    /// Receiver address
    #[arg(short, long, default_value_t = String::from("127.0.0.1:9000"))]
    receiver: String,

    /// Sender address
    #[arg(long, default_value_t = String::from("127.0.0.1:9001"))]
    sender: String,

    /// Peripheral address
    #[arg(short, long)]
    peripheral_address: Option<String>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    tracing_subscriber::fmt::init();

    let arguments = Arguments::parse();
    let socket = UdpSocket::bind(&arguments.sender).unwrap();
    info!("Binded to address {}", arguments.sender);

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

    // If the user passed a peripheral address, try to parse it.
    let maybe_peripheral_address = arguments.peripheral_address.and_then(|peripheral_address| {
        let delimiter = BDAddr::from_str_delim(&peripheral_address);
        let no_delimiter = BDAddr::from_str_no_delim(&peripheral_address);
        delimiter.or(no_delimiter).ok()
    });

    let peripheral = if let Some(peripheral_address) = maybe_peripheral_address {
        adapter.scan_for_peripheral(peripheral_address).await?
    } else {
        interactive_peripheral_scan(adapter).await?
    };

    let peripheral_properties = peripheral.properties().await?.unwrap();
    let peripheral_address = peripheral_properties.address;
    let peripheral_local_name = peripheral_properties
        .local_name
        .unwrap_or(String::from("(Empty)"));

    info!(
        "Connecting to {} [{}]",
        peripheral_local_name, peripheral_address
    );
    peripheral.connect().await?;
    info!(
        "Connected to {} [{}]",
        peripheral_local_name, peripheral_address
    );

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

    let time = Local::now().format("%Y%m%d-%H%M%S");
    let log_name = format!("{}.csv", time);
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
        socket.send_to(&buffer, &arguments.receiver).unwrap();
        info!(
            "Sent message to host [{}]: {:?}",
            arguments.receiver, message
        );

        let now = Local::now().to_rfc3339();
        let heart_rate = beats_per_minute.to_string();
        writer.write_record(&[&now, &heart_rate])?;
        writer.flush()?;
    }

    Ok(())
}

async fn interactive_peripheral_scan(adapter: &Adapter) -> Result<Peripheral> {
    loop {
        adapter.scan_for(1).await?;

        let peripherals = adapter.peripherals().await?;
        if peripherals.is_empty() {
            info!("No peripherals found, scanning again");
            continue;
        }

        let mut peripheral_selection_items = vec![String::from("[Scan again]")];
        let mut peripheral_local_names = get_peripheral_local_names(&peripherals)
            .await
            .iter()
            .map(|local_name| match local_name {
                Some(local_name) => local_name.clone(),
                None => String::from("(Empty)"),
            })
            .collect();
        peripheral_selection_items.append(&mut peripheral_local_names);

        let peripheral_selection = Select::with_theme(&ColorfulTheme::default())
            .with_prompt("Select bluetooth peripheral")
            .default(0)
            .items(&peripheral_selection_items)
            .interact()?;
        if peripheral_selection == 0 {
            info!("User chose to scan again");
            continue;
        }

        // Account for the "scan again" item.
        let peripheral_index = peripheral_selection - 1;

        match peripherals.get(peripheral_index).cloned() {
            Some(peripheral) => break Ok(peripheral),
            None => continue,
        }
    }
}

async fn get_peripheral_local_names(peripherals: &[Peripheral]) -> Vec<Option<String>> {
    join_all(
        peripherals
            .iter()
            .map(|peripheral| async {
                match peripheral.properties().await {
                    Err(_) => None,
                    Ok(properties) => {
                        if let Some(properties) = properties {
                            properties.local_name
                        } else {
                            None
                        }
                    }
                }
            })
            .collect::<Vec<_>>(),
    )
    .await
}
