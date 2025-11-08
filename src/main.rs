use elgato_streamdeck::{AsyncStreamDeck, DeviceStateUpdate, list_devices, new_hidapi};
use image::open;
use image::{DynamicImage, Rgb, imageops};
use std::collections::HashMap;
use std::io;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

const SOCKET_PATH: &str = "/tmp/rust-audio-monitor.sock";

// ... (send_audio_command function is unchanged) ...
async fn send_audio_command(command: &str) -> io::Result<String> {
    let stream = match UnixStream::connect(SOCKET_PATH).await {
        Ok(stream) => stream,
        Err(e) => {
            let msg = format!("Failed to connect to socket {}: {}", SOCKET_PATH, e);
            eprintln!("{}", msg);
            return Err(io::Error::new(io::ErrorKind::ConnectionRefused, msg));
        }
    };
    let (mut reader, mut writer) = stream.into_split();
    let cmd_with_newline = format!("{}\n", command);
    if let Err(e) = writer.write_all(cmd_with_newline.as_bytes()).await {
        eprintln!("Failed to write command: {}", e);
        return Err(e.into());
    }
    if let Err(e) = writer.shutdown().await {
        eprintln!("Failed to shutdown writer: {}", e);
        return Err(e.into());
    }
    let mut response = String::new();
    let mut buf_reader = BufReader::new(reader);
    buf_reader.read_line(&mut response).await?;
    Ok(response.trim().to_string())
}

// ... (create_fallback_image function is unchanged) ...
fn create_fallback_image(color: Rgb<u8>) -> DynamicImage {
    DynamicImage::ImageRgb8(image::RgbImage::from_fn(72, 72, move |_, _| color))
}

#[tokio::main]
async fn main() {
    let img_rec_off =
        open("src/rec_off.png").unwrap_or_else(|_| create_fallback_image(Rgb([80, 80, 80])));
    let img_rec_on =
        open("src/rec_on.png").unwrap_or_else(|_| create_fallback_image(Rgb([255, 0, 0])));

    match new_hidapi() {
        Ok(hid) => {
            for (kind, serial) in list_devices(&hid) {
                println!(
                    "Found Stream Deck: {:?} {} {}",
                    kind,
                    serial,
                    kind.product_id()
                );
                let device =
                    AsyncStreamDeck::connect(&hid, kind, &serial).expect("Failed to connect");

                device.set_brightness(50).await.unwrap();
                device.clear_all_button_images().await.unwrap();

                let mut button_files: HashMap<u8, String> = HashMap::new();
                button_files.insert(0, "/tmp/recording_A.wav".to_string());
                button_files.insert(1, "/tmp/recording_B.wav".to_string());
                // button_files.insert(2, "/tmp/recording_C.wav".to_string());

                // ‼️ This tracks which key, if any, is currently "live" (recording)
                let mut active_recording_key: Option<u8> = None;

                // Set initial button images to "off"
                for key in button_files.keys() {
                    device
                        .set_button_image(*key, img_rec_off.clone())
                        .await
                        .unwrap();
                }

                device.flush().await.unwrap();
                let reader = device.get_reader();

                'infinite: loop {
                    let updates = match reader.read(100.0).await {
                        Ok(updates) => updates,
                        Err(_) => break,
                    };

                    for update in updates {
                        match update {
                            DeviceStateUpdate::ButtonDown(key) => {
                                // ‼️ Only act if this is a mapped button
                                if let Some(filename) = button_files.get(&key) {
                                    println!("Button {} down...", key);

                                    // ‼️ 1. Check status first
                                    match send_audio_command("STATUS").await {
                                        Ok(status) => {
                                            // ‼️ 2. Only start if listening
                                            if status.contains("Listening") {
                                                println!(
                                                    "...Audio monitor is Listening. Sending START."
                                                );
                                                let cmd = format!("START {}", filename);

                                                // ‼️ 3. Send START
                                                match send_audio_command(&cmd).await {
                                                    Ok(_) => {
                                                        // ‼️ 4. Mark this key as active
                                                        active_recording_key = Some(key);
                                                        device
                                                            .set_button_image(
                                                                key,
                                                                img_rec_on.clone(),
                                                            )
                                                            .await
                                                            .unwrap();
                                                        device.flush().await.unwrap();
                                                        println!("...STARTED");
                                                    }
                                                    Err(e) => {
                                                        eprintln!(
                                                            "Failed to send START command: {}",
                                                            e
                                                        );
                                                    }
                                                }
                                            } else {
                                                // ‼️ Status was "Recording" or something else
                                                println!(
                                                    "...Audio monitor is NOT Listening (Status: {}). Ignoring press.",
                                                    status
                                                );
                                            }
                                        }
                                        Err(e) => {
                                            // ‼️ Failed to get status (socket down?)
                                            eprintln!(
                                                "Failed to get STATUS: {}. Ignoring press.",
                                                e
                                            );
                                        }
                                    }
                                }
                            }
                            DeviceStateUpdate::ButtonUp(key) => {
                                // ‼️ Exit if last button is pressed
                                if key == device.kind().key_count() - 1 {
                                    println!("Exit button pressed. Shutting down.");
                                    break 'infinite;
                                }

                                // ‼️ 5. Only STOP if this *specific* key is the active one
                                if active_recording_key == Some(key) {
                                    println!("Button {} up, sending STOP", key);

                                    match send_audio_command("STOP").await {
                                        Ok(_) => {
                                            // ‼️ 6. Clear active key and reset image
                                            active_recording_key = None;
                                            device
                                                .set_button_image(key, img_rec_off.clone())
                                                .await
                                                .unwrap();
                                            println!("...STOPPED");
                                        }
                                        Err(e) => {
                                            eprintln!("Failed to send STOP command: {}", e)
                                        }
                                    }
                                    device.flush().await.unwrap();
                                }
                                // ‼️ If active_recording_key is None or a *different* key,
                                // ‼️ this ButtonUp event is correctly ignored.
                            }
                            _ => {} // Ignore other events
                        }
                    }
                }
                drop(reader);
                println!("Cleaning up buttons...");
                device.clear_all_button_images().await.unwrap();
                device.flush().await.unwrap();
            }
        }
        Err(e) => eprintln!("Failed to create HidApi instance: {}", e),
    }
}
