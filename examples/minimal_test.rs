use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, Sample, SampleFormat, SampleRate, StreamConfig}; // Added BufferSize

fn minimal_cpal_output_test() -> Result<(), anyhow::Error> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .expect("No output device available");
    println!("[MinimalTest] Using output device: {}", device.name()?);

    let mut config = StreamConfig {
        channels: 1,                          // Target
        sample_rate: SampleRate(24000),       // Target
        buffer_size: BufferSize::Fixed(2400), // Target (100ms at 24kHz)
    };

    // You could also try to get a supported config first that matches SampleFormat::I16
    // and then try to set the sample_rate and buffer_size.
    // For this direct test, we assume the device *should* support this.

    println!(
        "[MinimalTest] Attempting to build stream with config: {:?}",
        config
    );

    let mut sample_clock = 0f32;
    let sample_rate_f32 = config.sample_rate.0 as f32;

    let stream = device.build_output_stream(
        &config,
        move |data: &mut [i16], _: &cpal::OutputCallbackInfo| {
            for frame in data.chunks_mut(config.channels as usize) {
                let value = ((sample_clock * 440.0 * 2.0 * std::f32::consts::PI / sample_rate_f32)
                    .sin()
                    * 0.5
                    * i16::MAX as f32) as i16;
                for sample in frame.iter_mut() {
                    *sample = value;
                }
                sample_clock = (sample_clock + 1.0) % sample_rate_f32;
            }
        },
        |err| eprintln!("[MinimalTest] CPAL stream error: {:?}", err),
        None,
    )?;
    stream.play()?;
    println!("[MinimalTest] Playing sine wave for 5 seconds...");
    std::thread::sleep(std::time::Duration::from_secs(5));
    println!("[MinimalTest] Done.");
    Ok(())
}

// To run this, you could put it in a new file like `examples/minimal_test.rs`
// and add `[[example]] name = "minimal_test" path = "examples/minimal_test.rs"` to Cargo.toml
// Then run `cargo run --example minimal_test`

fn main() {
    minimal_cpal_output_test().unwrap();
}
