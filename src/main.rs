use elgato_streamdeck::{list_devices, new_hidapi, AsyncStreamDeck, DeviceStateUpdate};
use elgato_streamdeck::images::convert_image_with_format;
use midir::os::unix::VirtualOutput;
use midir::MidiOutput;

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
                
                // 1. Clear all physical buttons (makes them black)
                device.clear_all_button_images().await.unwrap();

                // 2. Clear the LCD Screen (Stream Deck +) by filling it with a black image
                if let Some(format) = device.kind().lcd_image_format() {
                    // Create a completely black image matching the exact dimensions of the screen
                    let black_image = image::DynamicImage::ImageRgb8(image::RgbImage::new(
                        format.size.0 as u32,
                        format.size.1 as u32,
                    ));
                    
                    let converted_image = convert_image_with_format(format, black_image).unwrap();
                    let _ = device.write_lcd_fill(&converted_image).await;
                }

                println!("Key count: {}", kind.key_count());
                println!("Touch point count: {}", kind.touchpoint_count());
                
                // 3. Ensure touch display zones are set to black
                for i in 0..kind.touchpoint_count() {
                    device.set_touchpoint_color(i, 0, 0, 0).await.unwrap();
                }

                // Flush the black/clear state to the device
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
