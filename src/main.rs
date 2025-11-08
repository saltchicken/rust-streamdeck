use elgato_streamdeck::{AsyncStreamDeck, DeviceStateUpdate, list_devices, new_hidapi};
use image::open;
use image::{DynamicImage, Rgb, imageops};
use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
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
    // ‼️ 1. Load the new "play" image
    let img_play = open("src/play.png").unwrap_or_else(|_| create_fallback_image(Rgb([0, 255, 0]))); // Green fallback

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

                let mut active_recording_key: Option<u8> = None;

                // ‼️ 2. Set initial button images based on file existence
                for (key, filename) in &button_files {
                    let path = PathBuf::from(filename);
                    let initial_image = if path.exists() {
                        println!(
                            "File {} exists, setting 'play' icon for button {}",
                            filename, key
                        );
                        img_play.clone()
                    } else {
                        img_rec_off.clone()
                    };
                    device.set_button_image(*key, initial_image).await.unwrap();
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
                                if let Some(filename) = button_files.get(&key) {
                                    println!("Button {} down...", key);

                                    // Check for file existence first
                                    let path = PathBuf::from(filename);
                                    if path.exists() {
                                        eprintln!(
                                            "File {} already exists. Ignoring press.",
                                            filename
                                        );
                                        // ‼️ Button already shows "play", so we're done
                                        continue;
                                    }

                                    // (Rest of the ButtonDown logic is unchanged)
                                    match send_audio_command("STATUS").await {
                                        Ok(status) => {
                                            if status.contains("Listening") {
                                                println!(
                                                    "...Audio monitor is Listening. Sending START."
                                                );
                                                let cmd = format!("START {}", filename);

                                                match send_audio_command(&cmd).await {
                                                    Ok(_) => {
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
                                                println!(
                                                    "...Audio monitor is NOT Listening (Status: {}). Ignoring press.",
                                                    status
                                                );
                                            }
                                        }
                                        Err(e) => {
                                            eprintln!(
                                                "Failed to get STATUS: {}. Ignoring press.",
                                                e
                                            );
                                        }
                                    }
                                }
                            }
                            DeviceStateUpdate::ButtonUp(key) => {
                                if key == device.kind().key_count() - 1 {
                                    println!("Exit button pressed. Shutting down.");
                                    break 'infinite;
                                }

                                if active_recording_key == Some(key) {
                                    println!("Button {} up, sending STOP", key);

                                    match send_audio_command("STOP").await {
                                        Ok(_) => {
                                            active_recording_key = None;

                                            // ‼️ 3. On successful STOP, set image to "play"
                                            device
                                                .set_button_image(key, img_play.clone())
                                                .await
                                                .unwrap();
                                            println!("...STOPPED. File saved.");
                                        }
                                        Err(e) => {
                                            eprintln!(
                                                "Failed to send STOP command: {}. Try releasing and pressing again.",
                                                e
                                            );
                                            // ‼️ We don't clear active_recording_key,
                                            // so the button stays red.
                                            // Releasing and pressing again will retry the STOP.
                                        }
                                    }
                                    device.flush().await.unwrap();
                                }
                            }
                            _ => {}
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
