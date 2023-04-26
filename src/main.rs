use btleplug::api::{Central, Manager as _, Peripheral as _, ScanFilter};
use btleplug::platform::{Manager, Peripheral};
use clap::Parser;
use dialoguer::{theme::ColorfulTheme, Select};
use futures::future::join_all;
use futures::StreamExt;
use rosc::{encoder, OscMessage, OscPacket, OscType};
use std::error::Error;
use std::net::UdpSocket;
use std::time::{Duration, Instant};
use tokio::time;
use tracing::info;
use uuid::{uuid, Uuid};

const HEART_RATE_CHARACTERISTIC_UUID: Uuid = uuid!("00002a37-0000-1000-8000-00805f9b34fb");
const BATTERY_LEVEL_CHARACTERISTIC_UUID: Uuid = uuid!("00002a19-0000-1000-8000-00805f9b34fb");

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Arguments {
    /// Host address
    #[arg(long, default_value_t = String::from("127.0.0.1:9001"))]
    host: String,

    /// Client address
    #[arg(long, default_value_t = String::from("127.0.0.1:9000"))]
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
    let adpater_selection_items: Vec<_> = adapters
        .iter()
        .map(|adapter| format!("{:?}", adapter))
        .collect();
    let adapter_selection = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("Select bluetooth adapter")
        .default(0)
        .items(&adpater_selection_items)
        .interact()
        .unwrap();
    let adapter = adapters.get(adapter_selection).unwrap();

    let scan_duration_input: u64 = Input::new()
        .with_prompt("How many seconds do you want to scan for?")
        .with_initial_text("5".to_string())
        .interact_text()?;
    adapter.start_scan(ScanFilter::default()).await?;
    let scan_duration = Duration::from_secs(scan_duration_input);
    time::sleep(scan_duration).await;

    let peripherals = adapter.peripherals().await?;
    let peripheral_selection_items = join_all(
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
    .await;
    let peripheral_selection = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("Select bluetooth peripheral")
        .default(0)
        .items(&peripheral_selection_items)
        .interact()
        .unwrap();
    let peripheral = peripherals.get(peripheral_selection).unwrap();

    peripheral.connect().await?;
    let peripheral_local_name = peripheral.properties().await?.unwrap().local_name.unwrap();
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

    let chatbox_input_address = "/chatbox/input";

    peripheral.subscribe(heart_rate_characteristic).await?;
    let mut notification_stream = peripheral.notifications().await?;
    let mut last_message_instant = Instant::now();
    while let Some(data) = notification_stream.next().await {
        info!(
            "Received data from {} [{:?}]: {:?}",
            peripheral_local_name, data.uuid, data.value
        );
        if last_message_instant.elapsed().as_secs() > 2 {
            let beats_per_minute: u8 = data.value[1];
            let content = format!("HR: {:?}", beats_per_minute);
            let message = OscPacket::Message(OscMessage {
                addr: chatbox_input_address.to_string(),
                args: vec![
                    OscType::String(content),
                    OscType::Bool(true),
                    OscType::Bool(false),
                ],
            });
            let buffer = encoder::encode(&message).unwrap();
            socket.send_to(&buffer, &arguments.client).unwrap();
            info!(
                "Sent data to client [{:?}]: {:?}",
                arguments.client, message
            );
            last_message_instant = Instant::now()
        }
    }

    Ok(())
}
