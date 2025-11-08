use elgato_streamdeck::{AsyncStreamDeck, DeviceStateUpdate, list_devices, new_hidapi};
use image::open;
use image::{DynamicImage, Rgb, imageops};
use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

// ‼️ Add imports for Time and FileSystem
use std::fs;
use std::time::{Duration, Instant};
use tokio::process::Command;
//
const SOCKET_PATH: &str = "/tmp/rust-audio-monitor.sock";
const PLAYBACK_SINK_NAME: Option<&str> = Some("MyMixer");

async fn play_audio_file(path: &PathBuf) -> io::Result<()> {
    let player = "pw-play"; // ‼️ Assumes pw-play is in your PATH
    println!(
        "Attempting to play file with '{}': {}",
        player,
        path.display()
    );

    // Create the command
    let mut cmd = Command::new(player);
    if let Some(sink_name) = PLAYBACK_SINK_NAME {
        cmd.arg("--target");
        cmd.arg(sink_name);
        println!("...routing playback to sink: {}", sink_name);
    } else {
        println!("...routing playback to default output.");
    }
    cmd.arg(path);

    // Run the command and wait for its status
    // This runs in a spawned tokio task, so it won't block the UI
    let status = cmd.status().await?;

    if status.success() {
        println!("Playback successful.");
        Ok(())
    } else {
        // This will catch errors like "pw-play: command not found"
        let msg = format!(
            "Playback command '{}' failed with status: {}",
            player, status
        );
        eprintln!("{}", msg);
        Err(io::Error::new(io::ErrorKind::Other, msg))
    }
}

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
    let img_play = open("src/play.png").unwrap_or_else(|_| create_fallback_image(Rgb([0, 255, 0])));

    match new_hidapi() {
        Ok(hid) => {
            for (kind, serial) in list_devices(&hid) {
                // ... (device setup and button mapping is unchanged) ...
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

                let mut button_files: HashMap<u8, PathBuf> = HashMap::new();
                button_files.insert(0, PathBuf::from("/tmp/recording_A.wav"));
                button_files.insert(1, PathBuf::from("/tmp/recording_B.wav"));
                // button_files.insert(2, PathBuf::from("/tmp/recording_C.wav"));

                let mut active_recording_key: Option<u8> = None;
                let mut pending_delete: HashMap<u8, Instant> = HashMap::new();

                for (key, path) in &button_files {
                    let initial_image = if path.exists() {
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
                            // ... (ButtonDown logic is unchanged) ...
                            DeviceStateUpdate::ButtonDown(key) => {
                                if let Some(path) = button_files.get(&key) {
                                    if path.exists() {
                                        println!(
                                            "Button {} down (file exists). Holding for delete...",
                                            key
                                        );
                                        pending_delete.insert(key, Instant::now());
                                        device
                                            .set_button_image(key, img_rec_on.clone())
                                            .await
                                            .unwrap();
                                        device.flush().await.unwrap();
                                    } else {
                                        println!(
                                            "Button {} down (no file). Checking status...",
                                            key
                                        );
                                        match send_audio_command("STATUS").await {
                                            Ok(status) => {
                                                if status.contains("Listening") {
                                                    println!(
                                                        "...Audio monitor is Listening. Sending START."
                                                    );
                                                    let cmd =
                                                        format!("START {}", path.to_string_lossy());

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
                                                            eprintln!("Failed to send START: {}", e)
                                                        }
                                                    }
                                                } else {
                                                    println!(
                                                        "...Audio monitor is NOT Listening (Status: {}).",
                                                        status
                                                    );
                                                }
                                            }
                                            Err(e) => {
                                                eprintln!("Failed to get STATUS: {}.", e)
                                            }
                                        }
                                    }
                                }
                            }
                            DeviceStateUpdate::ButtonUp(key) => {
                                if key == device.kind().key_count() - 1 {
                                    println!("Exit button pressed. Shutting down.");
                                    break 'infinite;
                                }

                                // (Check 1: active_recording_key... unchanged)
                                if active_recording_key == Some(key) {
                                    println!("Button {} up, (was recording), sending STOP", key);
                                    match send_audio_command("STOP").await {
                                        Ok(_) => {
                                            active_recording_key = None;
                                            device
                                                .set_button_image(key, img_play.clone())
                                                .await
                                                .unwrap();
                                            println!("...STOPPED. File saved.");
                                        }
                                        Err(e) => {
                                            eprintln!("Failed to send STOP: {}", e);
                                        }
                                    }
                                    device.flush().await.unwrap();

                                // (Check 2: pending_delete... MODIFIED)
                                } else if let Some(start_time) = pending_delete.remove(&key) {
                                    let hold_duration = start_time.elapsed();
                                    println!(
                                        "Button {} up (was pending delete). Held for {:?}",
                                        key, hold_duration
                                    );

                                    if hold_duration >= Duration::from_secs(2) {
                                        // Held for > 2s: Delete the file
                                        // (This delete logic is unchanged)
                                        if let Some(path) = button_files.get(&key) {
                                            match fs::remove_file(path) {
                                                Ok(_) => {
                                                    println!("...File {} deleted.", path.display());
                                                    device
                                                        .set_button_image(key, img_rec_off.clone())
                                                        .await
                                                        .unwrap();
                                                }
                                                Err(e) => {
                                                    eprintln!(
                                                        "...Failed to delete file {}: {}",
                                                        path.display(),
                                                        e
                                                    );
                                                    device
                                                        .set_button_image(key, img_play.clone())
                                                        .await
                                                        .unwrap();
                                                }
                                            }
                                        }
                                    } else {
                                        // ‼️ Held for < 2s: Play the file
                                        println!("...Hold < 2s. Triggering playback.");
                                        if let Some(path) = button_files.get(&key) {
                                            // ‼️ Spawn playback in a new task
                                            // ‼️ so it doesn't block our event loop
                                            let path_clone = path.clone();
                                            tokio::spawn(async move {
                                                if let Err(e) = play_audio_file(&path_clone).await {
                                                    eprintln!("Playback failed: {}", e);
                                                }
                                            });
                                        }
                                        // ‼️ Set image back to "play"
                                        device
                                            .set_button_image(key, img_play.clone())
                                            .await
                                            .unwrap();
                                    }
                                    device.flush().await.unwrap();
                                }
                            }
                            _ => {}
                        }
                    }
                }
                drop(reader);
                // ... (cleanup code unchanged) ...
                println!("Cleaning up buttons...");
                device.clear_all_button_images().await.unwrap();
                device.flush().await.unwrap();
            }
        }
        Err(e) => eprintln!("Failed to create HidApi instance: {}", e),
    }
}
