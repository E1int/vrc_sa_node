use btleplug::api::{Central, Manager as _, Peripheral as _, ScanFilter};
use btleplug::platform::Manager;
use dialoguer::Input;
use dialoguer::{theme::ColorfulTheme, Select};
use futures::future::join_all;
use futures::StreamExt;
use rosc::{encoder, OscMessage, OscPacket, OscType};
use std::error::Error;
use std::net::{SocketAddrV4, UdpSocket};
use std::time::Duration;
use tokio::time;
use uuid::{uuid, Uuid};

const HEART_RATE_CHARACTERISTIC_UUID: Uuid = uuid!("00002a37-0000-1000-8000-00805f9b34fb");
const BATTERY_LEVEL_CHARACTERISTIC_UUID: Uuid = uuid!("00002a19-0000-1000-8000-00805f9b34fb");

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    tracing_subscriber::fmt::init();

    let host: SocketAddrV4 = Input::new()
        .with_prompt("Host address")
        .with_initial_text("127.0.0.1:9001".to_string())
        .interact_text()?;
    let client: SocketAddrV4 = Input::new()
        .with_prompt("Client address")
        .with_initial_text("127.0.0.1:9000".to_string())
        .interact_text()?;
    let socket = UdpSocket::bind(host).unwrap();

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
    println!("Connected to {:?}", peripheral);

    peripheral.discover_services().await?;
    let characteristics = peripheral.characteristics();
    let heartrate_characteristic = characteristics
        .iter()
        .find(|characteristic| characteristic.uuid == HEART_RATE_CHARACTERISTIC_UUID)
        .unwrap();

    let chatbox_input_address = "/chatbox/input";

    peripheral.subscribe(heartrate_characteristic).await?;
    let mut notification_stream = peripheral.notifications().await?;
    while let Some(data) = notification_stream.next().await {
        println!(
            "Received data from bluetooth peripheral [{:?}]: {:?}",
            data.uuid, data.value
        );
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
        socket.send_to(&buffer, client).unwrap();
        println!("Sent data to client [{:?}]: {:?}", client, message);
    }

    Ok(())
}
