// SPDX-License-Identifier: MPL-2.0

//! Passive meters for app-owned bus ports. These report samples, not DSP gate state.

use std::io::Cursor;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::Instant;

use cosmos_control::MeterSnapshot;
use pipewire as pw;
use pw::properties::properties;
use pw::spa::param::audio::{AudioFormat, AudioInfoRaw};
use pw::spa::param::format::{MediaSubtype, MediaType};
use pw::spa::param::{ParamType, format_utils};
use pw::spa::pod::{Object, Pod, Value, serialize::PodSerializer};

const FRESH_MS: u64 = 500;
const WAITING: u32 = 1;
const BAD_FORMAT: u32 = 2;
const NON_FINITE: u32 = 4;
const MISSING_CHUNK: u32 = 5;
const BAD_PLANES: u32 = 6;
const CORRUPTED_CHUNK: u32 = 7;
const UNKNOWN_CHUNK_FLAGS: u32 = 9;
const UNMAPPED_BUFFER: u32 = 10;
const BAD_STRIDE: u32 = 11;
const ZERO_SIZE: u32 = 12;
const PARTIAL_FRAME: u32 = 13;
const BAD_RANGE: u32 = 14;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Levels {
    peak: [f32; 2],
    rms: [f32; 2],
    frames: u32,
}

struct Shared {
    epoch: Instant,
    format_ok: AtomicBool,
    streaming: AtomicBool,
    failed: AtomicBool,
    sequence: AtomicU64,
    peak: [AtomicU32; 2],
    rms: [AtomicU32; 2],
    frames: AtomicU32,
    timestamp_ms: AtomicU64,
    error: AtomicU32,
}

impl Shared {
    fn new() -> Self {
        Self {
            epoch: Instant::now(),
            format_ok: AtomicBool::new(false),
            streaming: AtomicBool::new(false),
            failed: AtomicBool::new(false),
            sequence: AtomicU64::new(0),
            peak: std::array::from_fn(|_| AtomicU32::new(0)),
            rms: std::array::from_fn(|_| AtomicU32::new(0)),
            frames: AtomicU32::new(0),
            timestamp_ms: AtomicU64::new(0),
            error: AtomicU32::new(WAITING),
        }
    }

    fn state_changed(&self, state: &pw::stream::StreamState) {
        self.streaming.store(
            matches!(state, pw::stream::StreamState::Streaming),
            Ordering::Release,
        );
        // Listener registration emits no initial state. Unconnected therefore
        // means this stream was removed/disconnected after connect began.
        // Keep failure sticky until runtime replaces the entire meter.
        if matches!(
            state,
            pw::stream::StreamState::Error(_) | pw::stream::StreamState::Unconnected
        ) {
            self.failed.store(true, Ordering::Release);
        }
    }

    // Only the process callback writes this replaceable slot. SeqCst also orders
    // the payload against both sequence reads; readers never wait for the writer.
    fn publish(&self, result: Result<Levels, u32>) {
        let (levels, error) = match result {
            Ok(levels) => (levels, 0),
            Err(error) => (Levels::default(), error),
        };
        let timestamp = self.now_ms();
        self.sequence.fetch_add(1, Ordering::SeqCst);
        for channel in 0..2 {
            self.peak[channel].store(levels.peak[channel].to_bits(), Ordering::SeqCst);
            self.rms[channel].store(levels.rms[channel].to_bits(), Ordering::SeqCst);
        }
        self.frames.store(levels.frames, Ordering::SeqCst);
        self.error.store(error, Ordering::SeqCst);
        self.timestamp_ms.store(timestamp, Ordering::SeqCst);
        self.sequence.fetch_add(1, Ordering::SeqCst);
    }

    fn now_ms(&self) -> u64 {
        self.epoch.elapsed().as_millis().min(u64::MAX as u128) as u64
    }

    fn snapshot(&self, channels: u32) -> MeterSnapshot {
        // Bounded retries: a UI/control poll must not spin behind an RT writer.
        for _ in 0..64 {
            let before = self.sequence.load(Ordering::SeqCst);
            if before & 1 != 0 {
                std::hint::spin_loop();
                continue;
            }
            let peak = self
                .peak
                .each_ref()
                .map(|p| f32::from_bits(p.load(Ordering::SeqCst)));
            let rms = self
                .rms
                .each_ref()
                .map(|r| f32::from_bits(r.load(Ordering::SeqCst)));
            let frames = self.frames.load(Ordering::SeqCst);
            let timestamp = self.timestamp_ms.load(Ordering::SeqCst);
            let error = self.error.load(Ordering::SeqCst);
            if before != self.sequence.load(Ordering::SeqCst) {
                std::hint::spin_loop();
                continue;
            }
            let message = if self.failed.load(Ordering::Acquire) {
                Some("meter stream failed")
            } else if !self.format_ok.load(Ordering::Acquire) {
                Some("meter is waiting for supported mono/stereo F32 audio")
            } else {
                match error {
                    0 => None,
                    WAITING => Some("meter is waiting for samples"),
                    BAD_FORMAT => Some("meter received an unsupported audio format"),
                    NON_FINITE => Some("meter received a non-finite audio sample"),
                    MISSING_CHUNK => Some("meter audio buffer has no chunk metadata"),
                    BAD_PLANES => Some("meter expected one interleaved audio plane"),
                    CORRUPTED_CHUNK => Some("meter received a corrupted audio chunk"),
                    UNKNOWN_CHUNK_FLAGS => Some("meter received unknown audio chunk flags"),
                    UNMAPPED_BUFFER => Some("meter audio buffer is not mapped"),
                    BAD_STRIDE => Some("meter audio chunk has an invalid frame stride"),
                    ZERO_SIZE => Some("meter audio chunk contains no sample bytes"),
                    PARTIAL_FRAME => Some("meter audio chunk contains a partial frame"),
                    BAD_RANGE => Some("meter audio chunk exceeds the mapped byte range"),
                    _ => Some("meter received an invalid audio buffer"),
                }
            };
            let fresh = message.is_none()
                && self.streaming.load(Ordering::Acquire)
                && before != 0
                && frames != 0
                && self.now_ms().saturating_sub(timestamp) <= FRESH_MS;
            return MeterSnapshot {
                channels,
                peak,
                rms,
                frames,
                sequence: before / 2,
                fresh,
                silent: fresh && peak == [0.0; 2],
                error: message.map(str::to_owned),
            };
        }
        MeterSnapshot {
            channels,
            peak: [0.0; 2],
            rms: [0.0; 2],
            frames: 0,
            sequence: 0,
            fresh: false,
            silent: false,
            error: Some("meter snapshot is busy".to_owned()),
        }
    }
}

/// Owns one manually linked mono/stereo capture stream and its latest sample block.
/// Construct and poll on the PipeWire main loop; the process callback only uses
/// fixed stack data, lock-free atomics, and Linux's monotonic clock.
pub struct Meter {
    // Separate listeners keep main-loop format events and RT processing from
    // borrowing the same pipewire-rs listener user data on different threads.
    _process_listener: pw::stream::StreamListener<Arc<Shared>>,
    _control_listener: pw::stream::StreamListener<Arc<Shared>>,
    stream: pw::stream::StreamRc,
    shared: Arc<Shared>,
    node_name: String,
    channels: u32,
}

impl Meter {
    pub fn new(core: pw::core::CoreRc, node_name: &str, channels: u32) -> Result<Self, String> {
        let bus_id = node_name
            .strip_prefix("cosmos.bus.")
            .and_then(|name| {
                name.strip_suffix(".meter-input")
                    .or_else(|| name.strip_suffix(".meter-output"))
            })
            .filter(|id| {
                !id.is_empty()
                    && id.len() <= 64
                    && id.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')
                    })
            })
            .ok_or("meter node must name an app-owned bus meter")?;
        if !matches!(channels, 1 | 2) {
            return Err("meter supports mono or stereo only".to_owned());
        }
        let positions = if channels == 1 {
            "[ MONO ]"
        } else {
            "[ FL FR ]"
        };
        let stream = pw::stream::StreamRc::new(
            core,
            node_name,
            properties! {
                "node.name" => node_name,
                "node.description" => "Cosmos bus sample meter",
                "media.type" => "Audio",
                "media.category" => "Capture",
                "media.class" => "Stream/Input/Audio",
                "node.passive" => "true",
                "node.autoconnect" => "false",
                "node.dont-reconnect" => "true",
                "node.virtual" => "true",
                "stream.dont-remix" => "true",
                "audio.channels" => channels.to_string(),
                "audio.position" => positions,
                "adapter.auto-port-config" => "{ mode = dsp }",
                "cosmos.owner" => "cosmos-audio-mixer",
                "cosmos.bus.id" => bus_id,
            },
        )
        .map_err(|error| format!("create meter: {error}"))?;
        let shared = Arc::new(Shared::new());
        let control_listener = stream
            .add_local_listener_with_user_data(Arc::clone(&shared))
            .state_changed(|_, shared, _, state| {
                shared.state_changed(&state);
            })
            .param_changed(move |_, shared, id, parameter| {
                if id != ParamType::Format.as_raw() {
                    return;
                }
                let valid = parameter.is_some_and(|parameter| {
                    let mut info = AudioInfoRaw::new();
                    format_utils::parse_format(parameter)
                        .is_ok_and(|format| format == (MediaType::Audio, MediaSubtype::Raw))
                        && info.parse(parameter).is_ok()
                        && valid_format(&info, channels)
                });
                shared.format_ok.store(valid, Ordering::Release);
            })
            .register()
            .map_err(|error| format!("listen to meter format: {error}"))?;
        let process_listener = stream
            .add_local_listener_with_user_data(Arc::clone(&shared))
            .process(move |stream, shared| {
                let Some(mut buffer) = stream.dequeue_buffer() else {
                    return;
                };
                if !shared.format_ok.load(Ordering::Acquire) {
                    shared.publish(Err(BAD_FORMAT));
                    return;
                }
                let datas = buffer.datas_mut();
                let result = if let [data] = datas {
                    if data.as_raw().chunk.is_null() {
                        Err(MISSING_CHUNK)
                    } else {
                        let chunk = data.chunk();
                        let (offset, size, stride, flags) = (
                            chunk.offset(),
                            chunk.size(),
                            chunk.stride(),
                            chunk.as_raw().flags,
                        );
                        if flags & pw::spa::sys::SPA_CHUNK_FLAG_CORRUPTED as i32 != 0 {
                            Err(CORRUPTED_CHUNK)
                        } else if flags & !(pw::spa::sys::SPA_CHUNK_FLAG_EMPTY as i32) != 0 {
                            Err(UNKNOWN_CHUNK_FLAGS)
                        } else if flags != 0 {
                            // SPA EMPTY explicitly denotes neutral audio. Its
                            // sample storage need not contain readable PCM.
                            neutral_levels(data.as_raw().maxsize, channels, offset, size, stride)
                        } else if let Some(bytes) = data.data() {
                            measure(bytes, channels, offset, size, stride)
                        } else {
                            Err(UNMAPPED_BUFFER)
                        }
                    }
                } else {
                    Err(BAD_PLANES)
                };
                shared.publish(result);
                // Buffer's Drop returns it to this stream without allocating.
            })
            .register()
            .map_err(|error| format!("listen to meter samples: {error}"))?;
        let mut info = AudioInfoRaw::new();
        info.set_format(AudioFormat::F32LE);
        info.set_channels(channels);
        info.set_position(channel_positions(channels));
        let bytes = PodSerializer::serialize(
            Cursor::new(Vec::new()),
            &Value::Object(Object {
                type_: pw::spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
                id: ParamType::EnumFormat.as_raw(),
                properties: info.into(),
            }),
        )
        .map_err(|error| format!("serialize meter format: {error:?}"))?
        .0
        .into_inner();
        let pod = Pod::from_bytes(&bytes).ok_or("invalid meter format pod")?;
        stream
            .connect(
                pw::spa::utils::Direction::Input,
                None,
                pw::stream::StreamFlags::MAP_BUFFERS
                    | pw::stream::StreamFlags::RT_PROCESS
                    | pw::stream::StreamFlags::DONT_RECONNECT,
                &mut [pod],
            )
            .map_err(|error| format!("connect passive meter: {error}"))?;
        Ok(Self {
            _process_listener: process_listener,
            _control_listener: control_listener,
            stream,
            shared,
            node_name: node_name.to_owned(),
            channels,
        })
    }

    pub fn node_id(&self) -> u32 {
        self.stream.node_id()
    }

    pub fn node_name(&self) -> &str {
        &self.node_name
    }

    pub fn channels(&self) -> u32 {
        self.channels
    }

    pub fn failed(&self) -> bool {
        self.shared.failed.load(Ordering::Acquire)
    }

    pub fn snapshot(&self) -> MeterSnapshot {
        self.shared.snapshot(self.channels)
    }
}

fn channel_positions(channels: u32) -> [u32; pw::spa::sys::SPA_AUDIO_MAX_CHANNELS as usize] {
    let mut positions = [0; pw::spa::sys::SPA_AUDIO_MAX_CHANNELS as usize];
    if channels == 1 {
        positions[0] = pw::spa::sys::SPA_AUDIO_CHANNEL_MONO;
    } else {
        positions[0] = pw::spa::sys::SPA_AUDIO_CHANNEL_FL;
        positions[1] = pw::spa::sys::SPA_AUDIO_CHANNEL_FR;
    }
    positions
}

fn valid_format(info: &AudioInfoRaw, channels: u32) -> bool {
    matches!(channels, 1 | 2)
        && info.format() == AudioFormat::F32LE
        && info.channels() == channels
        && info.rate() > 0
        && info.flags().is_empty()
        && info.position()[..channels as usize] == channel_positions(channels)[..channels as usize]
}

/// Decode only a validated interleaved F32LE plane, without alignment casts.
fn measure(
    bytes: &[u8],
    channels: u32,
    offset: u32,
    size: u32,
    stride: i32,
) -> Result<Levels, u32> {
    let (range, stride) = chunk_layout(bytes.len(), channels, offset, size, stride)?;
    let samples = &bytes[range];
    let mut levels = Levels {
        frames: (samples.len() / stride) as u32,
        ..Levels::default()
    };
    let mut squares = [0.0_f64; 2];
    for frame in samples.chunks_exact(stride) {
        for (channel, square) in squares.iter_mut().enumerate().take(channels as usize) {
            let start = channel * 4;
            let sample = f32::from_le_bytes([
                frame[start],
                frame[start + 1],
                frame[start + 2],
                frame[start + 3],
            ]);
            if !sample.is_finite() {
                return Err(NON_FINITE);
            }
            levels.peak[channel] = levels.peak[channel].max(sample.abs());
            *square += f64::from(sample) * f64::from(sample);
        }
    }
    for (channel, square) in squares.iter().enumerate().take(channels as usize) {
        levels.rms[channel] = (square / f64::from(levels.frames)).sqrt() as f32;
    }
    Ok(levels)
}

fn neutral_levels(
    maxsize: u32,
    channels: u32,
    offset: u32,
    size: u32,
    stride: i32,
) -> Result<Levels, u32> {
    let (range, stride) = chunk_layout(maxsize as usize, channels, offset, size, stride)?;
    Ok(Levels {
        frames: (range.len() / stride) as u32,
        ..Levels::default()
    })
}

fn chunk_layout(
    maxsize: usize,
    channels: u32,
    offset: u32,
    size: u32,
    stride: i32,
) -> Result<(std::ops::Range<usize>, usize), u32> {
    if !matches!(channels, 1 | 2) {
        return Err(BAD_FORMAT);
    }
    let frame_bytes = channels as usize * 4;
    let stride = usize::try_from(stride).map_err(|_| BAD_STRIDE)?;
    let size = size as usize;
    if stride < frame_bytes {
        return Err(BAD_STRIDE);
    }
    if size == 0 {
        return Err(ZERO_SIZE);
    }
    if !size.is_multiple_of(stride) {
        return Err(PARTIAL_FRAME);
    }
    let start = offset as usize;
    let end = start.checked_add(size).ok_or(BAD_RANGE)?;
    if end > maxsize {
        return Err(BAD_RANGE);
    }
    Ok((start..end, stride))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn padded_stereo_with_byte_offset_reports_actual_peak_and_rms() {
        let mut bytes = vec![99];
        for frame in [[0.25_f32, -0.5], [-0.25, 1.5]] {
            bytes.extend(frame.into_iter().flat_map(f32::to_le_bytes));
            bytes.extend([99; 4]);
        }
        bytes.push(99);
        let levels = measure(&bytes, 2, 1, 24, 12).unwrap();
        assert_eq!(levels.peak, [0.25, 1.5]);
        assert_eq!(levels.rms[0], 0.25);
        assert!((levels.rms[1] - 1.25_f32.sqrt()).abs() < 0.000001);
        assert_eq!(levels.frames, 2);
    }

    #[test]
    fn malformed_layouts_and_non_finite_samples_are_unavailable() {
        let bytes = [0; 16];
        for (channels, offset, size, stride) in [
            (0, 0, 8, 4),
            (3, 0, 12, 12),
            (2, 0, 8, 4),
            (1, 0, 4, 0),
            (1, 0, 4, -4),
            (1, 0, 0, 4),
            (2, 0, 7, 8),
            (1, 15, 4, 4),
            (1, u32::MAX, 4, 4),
        ] {
            assert!(measure(&bytes, channels, offset, size, stride).is_err());
        }
        for sample in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert_eq!(measure(&sample.to_le_bytes(), 1, 0, 4, 4), Err(NON_FINITE));
        }
    }

    #[test]
    fn mono_full_range_stays_finite_and_preserves_unused_channel() {
        let levels = measure(&f32::MAX.to_le_bytes(), 1, 0, 4, 4).unwrap();
        assert_eq!(
            levels,
            Levels {
                peak: [f32::MAX, 0.0],
                rms: [f32::MAX, 0.0],
                frames: 1
            }
        );
    }

    #[test]
    fn freshness_and_silence_require_valid_current_samples() {
        let shared = Shared::new();
        assert!(!shared.snapshot(1).silent);
        shared.format_ok.store(true, Ordering::Release);
        shared.streaming.store(true, Ordering::Release);
        shared.publish(measure(&[0; 8], 1, 0, 8, 4));
        let snapshot = shared.snapshot(1);
        assert!(snapshot.fresh && snapshot.silent && snapshot.error.is_none());
        shared.streaming.store(false, Ordering::Release);
        assert!(!shared.snapshot(1).silent);
        shared.publish(Err(NON_FINITE));
        assert!(shared.snapshot(1).error.is_some());
    }

    #[test]
    fn negotiation_rejects_unknown_formats_rates_and_channel_order() {
        let mut info = AudioInfoRaw::new();
        info.set_format(AudioFormat::F32LE);
        info.set_channels(2);
        info.set_rate(48000);
        info.set_position(channel_positions(2));
        assert!(valid_format(&info, 2));
        assert!(!valid_format(&info, 1));
        info.set_rate(0);
        assert!(!valid_format(&info, 2));
        info.set_rate(48000);
        info.set_format(AudioFormat::F32P);
        assert!(!valid_format(&info, 2));
        info.set_format(AudioFormat::F32LE);
        let mut reversed = channel_positions(2);
        reversed.swap(0, 1);
        info.set_position(reversed);
        assert!(!valid_format(&info, 2));
    }

    #[test]
    fn stale_samples_and_busy_slot_do_not_claim_silence() {
        let mut shared = Shared::new();
        shared.epoch = Instant::now() - std::time::Duration::from_secs(1);
        shared.format_ok.store(true, Ordering::Release);
        shared.streaming.store(true, Ordering::Release);
        shared.publish(measure(&[0; 4], 1, 0, 4, 4));
        shared.timestamp_ms.store(0, Ordering::SeqCst);
        let stale = shared.snapshot(1);
        assert!(!stale.fresh && !stale.silent);
        shared.sequence.store(3, Ordering::SeqCst);
        let busy = shared.snapshot(1);
        assert!(!busy.fresh && !busy.silent && busy.error.is_some());
    }

    #[test]
    fn empty_audio_validates_frame_layout_without_reading_sample_storage() {
        assert_eq!(
            neutral_levels(32, 2, 8, 24, 8),
            Ok(Levels {
                peak: [0.0; 2],
                rms: [0.0; 2],
                frames: 3
            })
        );
        assert_eq!(neutral_levels(32, 2, 8, 24, 0), Err(BAD_STRIDE));
        assert_eq!(neutral_levels(32, 2, 8, 23, 8), Err(PARTIAL_FRAME));
        assert_eq!(neutral_levels(32, 2, 9, 24, 8), Err(BAD_RANGE));
        assert_eq!(neutral_levels(32, 2, 8, 0, 8), Err(ZERO_SIZE));
    }

    #[test]
    fn failure_stays_visible_after_error_cleanup_and_remote_removal() {
        use pw::stream::StreamState;

        let shared = Shared::new();
        assert!(!shared.failed.load(Ordering::Acquire));
        for state in [
            StreamState::Connecting,
            StreamState::Paused,
            StreamState::Streaming,
        ] {
            shared.state_changed(&state);
            assert!(!shared.failed.load(Ordering::Acquire));
        }
        shared.state_changed(&StreamState::Error("stream failed".to_owned()));
        for state in [
            StreamState::Unconnected,
            StreamState::Connecting,
            StreamState::Paused,
        ] {
            shared.state_changed(&state);
            assert_eq!(
                shared.snapshot(1).error.as_deref(),
                Some("meter stream failed")
            );
        }
        let removed = Shared::new();
        removed.state_changed(&StreamState::Streaming);
        removed.state_changed(&StreamState::Unconnected);
        assert_eq!(
            removed.snapshot(1).error.as_deref(),
            Some("meter stream failed")
        );
    }
}
