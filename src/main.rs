use image::open;
use std::time::Duration;

use elgato_streamdeck::images::{convert_image_with_format, ImageRect};
use elgato_streamdeck::{list_devices, new_hidapi, AsyncStreamDeck, DeviceStateUpdate};
use midir::os::unix::VirtualOutput;
use midir::MidiOutput;
use tokio::time::sleep;

#[tokio::main]
async fn main() {
    // 1. Initialize MIDI output
    let midi_out = MidiOutput::new("Stream Deck MIDI").expect("Failed to create MIDI output");
    
    // On Linux/macOS, we can create a virtual MIDI device DAWs can connect to.
    #[cfg(not(target_os = "windows"))]
    let mut midi_conn = midi_out
        .create_virtual("StreamDeck Encoders")
        .expect("Failed to create virtual MIDI port");

    // On Windows, virtual ports aren't natively supported by midir, so we connect to the first available port.
    #[cfg(target_os = "windows")]
    let mut midi_conn = {
        let ports = midi_out.ports();
        let port = ports.first().expect("No MIDI output ports found. Please install a loopback driver like loopMIDI.");
        midi_out.connect(port, "StreamDeck Encoders").expect("Failed to connect to MIDI port")
    };

    // Array to track the current 0-127 values of the encoders. We'll default them to 64 (center).
    let mut encoder_values = [64u8; 8];

    // Create instance of HidApi
    match new_hidapi() {
        Ok(hid) => {
            // Refresh device list
            for (kind, serial) in list_devices(&hid) {
                println!("{:?} {} {}", kind, serial, kind.product_id());

                // Connect to the device
                let device =
                    AsyncStreamDeck::connect(&hid, kind, &serial).expect("Failed to connect");
                // Print out some info from the device
                println!(
                    "Connected to '{}' with version '{}'",
                    device.serial_number().await.unwrap(),
                    device.firmware_version().await.unwrap()
                );

                device.set_brightness(50).await.unwrap();
                device.clear_all_button_images().await.unwrap();

                // Use image-rs to load an image
                let image = open("src/no-place-like-localhost.jpg").unwrap();
                let alternative = image.grayscale().brighten(-50);

                println!("Key count: {}", kind.key_count());
                // Write it to the device
                for i in 0..kind.key_count() {
                    device.set_button_image(i, image.clone()).await.unwrap();
                }

                println!("Touch point count: {}", kind.touchpoint_count());
                for i in 0..kind.touchpoint_count() {
                    device.set_touchpoint_color(i, 255, 255, 255).await.unwrap();
                }

                if let Some(format) = device.kind().lcd_image_format() {
                    let scaled_image = image.clone().resize_to_fill(
                        format.size.0 as u32,
                        format.size.1 as u32,
                        image::imageops::FilterType::Nearest,
                    );
                    let converted_image = convert_image_with_format(format, scaled_image).unwrap();
                    let _ = device.write_lcd_fill(&converted_image).await;
                }

                let small = match device.kind().lcd_strip_size() {
                    Some((w, h)) => {
                        let min = w.min(h) as u32;
                        let scaled_image =
                            image.clone().resize_to_fill(min, min, image::imageops::Nearest);
                        Some(ImageRect::from_image(scaled_image).unwrap())
                    }
                    None => None,
                };

                // Flush
                device.flush().await.unwrap();

                // Start new task to animate the button images
                let device_clone = device.clone();
                tokio::spawn(async move {
                    let mut index = 0;
                    let mut previous = 0;

                    loop {
                        device_clone
                            .set_button_image(index, image.clone())
                            .await
                            .unwrap();
                        device_clone
                            .set_button_image(previous, alternative.clone())
                            .await
                            .unwrap();

                        device_clone.flush().await.unwrap();

                        sleep(Duration::from_secs_f32(0.5)).await;

                        previous = index;

                        index += 1;
                        if index >= kind.key_count() {
                            index = 0;
                        }
                    }
                });

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
                                if key == device.kind().key_count() - 1 {
                                    break 'infinite;
                                }
                            }
                            DeviceStateUpdate::EncoderTwist(dial, ticks) => {
                                // Calculate the new CC value using the tick delta, clamping to MIDI bounds
                                let dial_idx = dial as usize;
                                let current_val = encoder_values[dial_idx] as i32;
                                let new_val = (current_val + ticks as i32).clamp(0, 127) as u8;
                                
                                // Save state
                                encoder_values[dial_idx] = new_val;
                                
                                // Send MIDI CC message
                                // Status byte 0xB0 = Control Change on MIDI Channel 1
                                // We offset the CC number by 16 so dial 0 -> CC 16, dial 1 -> CC 17, etc.
                                let cc_number = 16 + dial as u8;
                                
                                if let Err(e) = midi_conn.send(&[0xB0, cc_number, new_val]) {
                                    eprintln!("Failed to send MIDI message: {}", e);
                                }
                                
                                println!("Dial {} twisted by {}. Sent CC {}: {}", dial, ticks, cc_number, new_val);
                            }
                            DeviceStateUpdate::EncoderDown(dial) => {
                                println!("Dial {} down", dial);
                            }
                            DeviceStateUpdate::EncoderUp(dial) => {
                                println!("Dial {} up", dial);
                            }

                            DeviceStateUpdate::TouchPointDown(point) => {
                                println!("Touch point {} down", point);
                            }
                            DeviceStateUpdate::TouchPointUp(point) => {
                                println!("Touch point {} up", point);
                            }

                            DeviceStateUpdate::TouchScreenPress(x, y) => {
                                println!("Touch Screen press at {x}, {y}");
                                if let Some(small) = &small {
                                    device.write_lcd(x, y, small).await.unwrap();
                                }
                            }

                            DeviceStateUpdate::TouchScreenLongPress(x, y) => {
                                println!("Touch Screen long press at {x}, {y}")
                            }

                            DeviceStateUpdate::TouchScreenSwipe((sx, sy), (ex, ey)) => {
                                println!("Touch Screen swipe from {sx}, {sy} to {ex}, {ey}")
                            }
                        }
                    }
                }

                drop(reader);
            }
        }
        Err(e) => eprintln!("Failed to create HidApi instance: {}", e),
    }
}
