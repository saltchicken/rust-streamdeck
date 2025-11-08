use elgato_streamdeck::{AsyncStreamDeck, DeviceStateUpdate, list_devices, new_hidapi};
use image::open;
use image::{DynamicImage, Rgb, imageops}; // ‼️ For fallback images
use std::collections::HashMap; // ‼️ To track button state
use std::io; // ‼️ For our new IPC function
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader}; // ‼️ For IPC
use tokio::net::UnixStream; // ‼️ The Unix Socket client

// ‼️ The path to the socket our audio monitor is listening on
const SOCKET_PATH: &str = "/tmp/rust-audio-monitor.sock";

// ‼️ This function connects to the socket, sends a command, and handles the connection.
// ‼️ It's built to mimic `socat` by shutting down the write-half of the
// ‼️ connection, which signals "End of File" (EOF) to the server's `read_line`.
async fn send_audio_command(command: &str) -> io::Result<String> {
    let stream = match UnixStream::connect(SOCKET_PATH).await {
        Ok(stream) => stream,
        Err(e) => {
            let msg = format!("Failed to connect to socket {}: {}", SOCKET_PATH, e);
            eprintln!("{}", msg);
            return Err(io::Error::new(io::ErrorKind::ConnectionRefused, msg));
        }
    };

    // ‼️ Split the stream so we can shut down writing after we send
    let (mut reader, mut writer) = stream.into_split();

    // ‼️ Send the command (with a newline!)
    let cmd_with_newline = format!("{}\n", command);
    if let Err(e) = writer.write_all(cmd_with_newline.as_bytes()).await {
        eprintln!("Failed to write command: {}", e);
        return Err(e.into());
    }

    // ‼️ Shut down the write-half. This is crucial.
    if let Err(e) = writer.shutdown().await {
        eprintln!("Failed to shutdown writer: {}", e);
        return Err(e.into());
    }

    // ‼️ Read the response back (e.g., for "STATUS")
    let mut response = String::new();
    let mut buf_reader = BufReader::new(reader);
    buf_reader.read_line(&mut response).await?;

    Ok(response.trim().to_string())
}

// ‼️ Helper to create a fallback image if one isn't found
fn create_fallback_image(color: Rgb<u8>) -> DynamicImage {
    DynamicImage::ImageRgb8(image::RgbImage::from_fn(72, 72, move |_, _| color))
}

#[tokio::main]
async fn main() {
    // ‼️ Load our button icons. Provides a fallback if not found.
    let img_rec_off =
        open("src/rec_off.png").unwrap_or_else(|_| create_fallback_image(Rgb([80, 80, 80]))); // Dark gray
    let img_rec_on =
        open("src/rec_on.png").unwrap_or_else(|_| create_fallback_image(Rgb([255, 0, 0]))); // Bright red

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

                // ‼️ Map button keys to their recording filenames
                let mut button_files: HashMap<u8, String> = HashMap::new();
                button_files.insert(0, "/tmp/recording_A.wav".to_string());
                button_files.insert(1, "/tmp/recording_B.wav".to_string());
                // ‼️ Add more buttons here
                // button_files.insert(2, "/tmp/recording_C.wav".to_string());

                // ‼️ Track the recording state (true = recording)
                let mut button_states: HashMap<u8, bool> = HashMap::new();

                // ‼️ Set initial button images to "off"
                for key in button_files.keys() {
                    device
                        .set_button_image(*key, img_rec_off.clone())
                        .await
                        .unwrap();
                    button_states.insert(*key, false); // ‼️ Init state
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
                                println!("Button {} down", key);
                            }
                            DeviceStateUpdate::ButtonUp(key) => {
                                println!("Button {} up", key);

                                // ‼️ Exit if last button is pressed
                                if key == device.kind().key_count() - 1 {
                                    break 'infinite;
                                }

                                // ‼️ Check if this is a button we've assigned a file to
                                if let Some(filename) = button_files.get(&key) {
                                    let is_recording = button_states.entry(key).or_insert(false);

                                    if *is_recording {
                                        // ‼️ We are recording, so send STOP
                                        println!("...is recording, sending STOP");
                                        match send_audio_command("STOP").await {
                                            Ok(_) => {
                                                *is_recording = false; // Update state
                                                // ‼️ Set image to OFF
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
                                    } else {
                                        // ‼️ Not recording, so send START
                                        let cmd = format!("START {}", filename);
                                        println!("...sending START for {}", filename);
                                        match send_audio_command(&cmd).await {
                                            Ok(_) => {
                                                *is_recording = true; // Update state
                                                // ‼️ Set image to ON
                                                device
                                                    .set_button_image(key, img_rec_on.clone())
                                                    .await
                                                    .unwrap();
                                                println!("...STARTED");
                                            }
                                            Err(e) => {
                                                eprintln!("Failed to send START command: {}", e)
                                            }
                                        }
                                    }
                                    device.flush().await.unwrap(); // ‼️ Flush image change
                                }
                            }
                            // ... (other device states are ignored for this example)
                            _ => {}
                        }
                    }
                }
                drop(reader);
            }
        }
        Err(e) => eprintln!("Failed to create HidApi instance: {}", e),
    }
}
