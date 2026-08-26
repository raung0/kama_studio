use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use anyhow::{Context, Result};
use ffmpeg::{
    codec, encoder, filter, format, frame, media, software::scaling, Dictionary, Packet, Rational,
};
use ffmpeg_next as ffmpeg;

use super::{AudioCodec, RenderSpec, VideoCodec};

pub(super) fn transcode(
    chunks: &[PathBuf],
    audio: Option<&Path>,
    output_path: &Path,
    settings: &RenderSpec,
    canvas_size: [u32; 2],
    fps: f64,
    cancelled: &Arc<AtomicBool>,
) -> Result<()> {
    crate::runtime::media::init_ffmpeg()?;
    let first = chunks.first().context("render cache is empty")?;
    let first_input = format::input(first)?;
    let first_stream = first_input
        .streams()
        .best(media::Type::Video)
        .context("cache has no video")?;
    let first_decoder = codec::context::Context::from_parameters(first_stream.parameters())?
        .decoder()
        .video()?;
    let target = settings.preset.resolution.dimensions(canvas_size);
    let fps_rate = Rational((fps.max(1.0) * 1_000.0).round() as i32, 1_000);
    let video_time_base = Rational(fps_rate.1, fps_rate.0);
    let mut output = format::output(output_path)?;
    let global = output
        .format()
        .flags()
        .contains(format::Flags::GLOBAL_HEADER);
    let (codec, mut video, pixel) =
        open_video_encoder(settings, target, fps_rate, video_time_base, global)?;
    let video_stream = {
        let mut stream = output.add_stream(codec)?;
        stream.set_parameters(&video);
        stream.set_time_base(video_time_base);
        stream.set_rate(fps_rate);
        stream.index()
    };

    let mut audio_input = if settings.preset.include_audio {
        Some(format::input(audio.context("rendered audio is missing")?)?)
    } else {
        None
    };
    let mut audio_state = if let Some(input) = audio_input.as_mut() {
        Some(AudioState::new(input, &mut output, settings, global)?)
    } else {
        None
    };

    output.write_header()?;
    let video_output_tb = output
        .stream(video_stream)
        .context("video stream disappeared")?
        .time_base();
    if let Some(state) = audio_state.as_mut() {
        state.output_time_base = output
            .stream(state.output_stream)
            .context("audio stream disappeared")?
            .time_base();
    }

    let mut scaler = scaling::Context::get(
        first_decoder.format(),
        first_decoder.width(),
        first_decoder.height(),
        pixel,
        target[0],
        target[1],
        scaling::flag::Flags::LANCZOS,
    )?;
    drop(first_decoder);
    drop(first_input);
    let mut pts = 0i64;
    for path in chunks {
        anyhow::ensure!(!cancelled.load(Ordering::Acquire), "transcode cancelled");
        let mut input = format::input(path)?;
        let stream = input
            .streams()
            .best(media::Type::Video)
            .context("cache chunk has no video")?;
        let stream_index = stream.index();
        let mut decoder = codec::context::Context::from_parameters(stream.parameters())?
            .decoder()
            .video()?;
        for (packet_stream, packet) in input.packets() {
            if packet_stream.index() != stream_index {
                continue;
            }
            decoder.send_packet(&packet)?;
            drain_video_decoder(
                &mut decoder,
                &mut scaler,
                &mut video,
                &mut output,
                video_stream,
                video_time_base,
                video_output_tb,
                &mut pts,
                cancelled,
            )?;
        }
        decoder.send_eof()?;
        drain_video_decoder(
            &mut decoder,
            &mut scaler,
            &mut video,
            &mut output,
            video_stream,
            video_time_base,
            video_output_tb,
            &mut pts,
            cancelled,
        )?;
    }
    video.send_eof()?;
    drain_video_packets(
        &mut video,
        &mut output,
        video_stream,
        video_time_base,
        video_output_tb,
    )?;

    if let (Some(input), Some(state)) = (audio_input.as_mut(), audio_state.as_mut()) {
        state.run(input, &mut output, cancelled)?;
    }
    output.write_trailer()?;
    Ok(())
}

fn drain_video_decoder(
    decoder: &mut codec::decoder::Video,
    scaler: &mut scaling::Context,
    encoder: &mut codec::encoder::video::Encoder,
    output: &mut format::context::Output,
    stream: usize,
    encoder_tb: Rational,
    output_tb: Rational,
    pts: &mut i64,
    cancelled: &Arc<AtomicBool>,
) -> Result<()> {
    let mut decoded = frame::Video::empty();
    while decoder.receive_frame(&mut decoded).is_ok() {
        anyhow::ensure!(!cancelled.load(Ordering::Acquire), "transcode cancelled");
        let mut scaled = frame::Video::new(encoder.format(), encoder.width(), encoder.height());
        scaler.run(&decoded, &mut scaled)?;
        scaled.set_pts(Some(*pts));
        *pts += 1;
        encoder.send_frame(&scaled)?;
        drain_video_packets(encoder, output, stream, encoder_tb, output_tb)?;
    }
    Ok(())
}

fn drain_video_packets(
    encoder: &mut codec::encoder::video::Encoder,
    output: &mut format::context::Output,
    stream: usize,
    encoder_tb: Rational,
    output_tb: Rational,
) -> Result<()> {
    let mut packet = Packet::empty();
    while encoder.receive_packet(&mut packet).is_ok() {
        packet.set_stream(stream);
        packet.rescale_ts(encoder_tb, output_tb);
        packet.write_interleaved(output)?;
    }
    Ok(())
}

struct AudioState {
    input_stream: usize,
    output_stream: usize,
    decoder: codec::decoder::Audio,
    encoder: codec::encoder::audio::Encoder,
    graph: filter::Graph,
    input_time_base: Rational,
    encoder_time_base: Rational,
    output_time_base: Rational,
}

impl AudioState {
    fn new(
        input: &mut format::context::Input,
        output: &mut format::context::Output,
        settings: &RenderSpec,
        global: bool,
    ) -> Result<Self> {
        let stream = input
            .streams()
            .best(media::Type::Audio)
            .context("audio mix has no audio stream")?;
        let input_stream = stream.index();
        let input_time_base = stream.time_base();
        let decoder = codec::context::Context::from_parameters(stream.parameters())?
            .decoder()
            .audio()?;
        let codec = audio_codec(settings.preset.audio_codec)?;
        let audio_codec = codec.audio()?;
        let mut audio = codec::context::Context::new_with_codec(codec)
            .encoder()
            .audio()?;
        let layout = ffmpeg::channel_layout::ChannelLayout::STEREO;
        let sample_format = audio_codec
            .formats()
            .and_then(|mut formats| formats.next())
            .context("audio encoder exposes no sample format")?;
        let rate = settings.preset.sample_rate.max(8_000) as i32;
        if global {
            audio.set_flags(codec::Flags::GLOBAL_HEADER);
        }
        audio.set_rate(rate);
        audio.set_channel_layout(layout);
        audio.set_format(sample_format);
        audio.set_time_base((1, rate));
        audio.set_bit_rate(settings.preset.audio_bitrate_kbps.max(64) as usize * 1_000);
        let encoder = audio.open_as(codec)?;
        let output_stream = {
            let mut stream = output.add_stream(codec)?;
            stream.set_parameters(&encoder);
            stream.set_time_base((1, rate));
            stream.index()
        };
        let mut graph = filter::Graph::new();
        let args = format!(
            "time_base={}:sample_rate={}:sample_fmt={}:channel_layout=0x{:x}",
            decoder.time_base(),
            decoder.rate(),
            decoder.format().name(),
            decoder.channel_layout().bits()
        );
        graph.add(
            &filter::find("abuffer").context("abuffer unavailable")?,
            "in",
            &args,
        )?;
        graph.add(
            &filter::find("abuffersink").context("abuffersink unavailable")?,
            "out",
            "",
        )?;
        {
            let mut sink = graph.get("out").context("audio sink missing")?;
            sink.set_sample_format(encoder.format());
            sink.set_channel_layout(encoder.channel_layout());
            sink.set_sample_rate(encoder.rate());
        }
        graph.output("in", 0)?.input("out", 0)?.parse("anull")?;
        graph.validate()?;
        if !audio_codec
            .capabilities()
            .contains(codec::capabilities::Capabilities::VARIABLE_FRAME_SIZE)
        {
            graph
                .get("out")
                .context("audio sink missing")?
                .sink()
                .set_frame_size(encoder.frame_size());
        }
        Ok(Self {
            input_stream,
            output_stream,
            decoder,
            encoder,
            graph,
            input_time_base,
            encoder_time_base: Rational(1, rate),
            output_time_base: Rational(1, rate),
        })
    }

    fn run(
        &mut self,
        input: &mut format::context::Input,
        output: &mut format::context::Output,
        cancelled: &Arc<AtomicBool>,
    ) -> Result<()> {
        for (stream, mut packet) in input.packets() {
            anyhow::ensure!(!cancelled.load(Ordering::Acquire), "transcode cancelled");
            if stream.index() != self.input_stream {
                continue;
            }
            packet.rescale_ts(stream.time_base(), self.input_time_base);
            self.decoder.send_packet(&packet)?;
            self.drain(output)?;
        }
        self.decoder.send_eof()?;
        self.drain(output)?;
        self.graph
            .get("in")
            .context("audio source missing")?
            .source()
            .flush()?;
        self.drain_filter(output)?;
        self.encoder.send_eof()?;
        self.drain_packets(output)
    }
    fn drain(&mut self, output: &mut format::context::Output) -> Result<()> {
        let mut decoded = frame::Audio::empty();
        while self.decoder.receive_frame(&mut decoded).is_ok() {
            self.graph
                .get("in")
                .context("audio source missing")?
                .source()
                .add(&decoded)?;
            self.drain_filter(output)?;
        }
        Ok(())
    }
    fn drain_filter(&mut self, output: &mut format::context::Output) -> Result<()> {
        let mut filtered = frame::Audio::empty();
        while self
            .graph
            .get("out")
            .context("audio sink missing")?
            .sink()
            .frame(&mut filtered)
            .is_ok()
        {
            self.encoder.send_frame(&filtered)?;
            self.drain_packets(output)?;
        }
        Ok(())
    }
    fn drain_packets(&mut self, output: &mut format::context::Output) -> Result<()> {
        let mut packet = Packet::empty();
        while self.encoder.receive_packet(&mut packet).is_ok() {
            packet.set_stream(self.output_stream);
            packet.rescale_ts(self.encoder_time_base, self.output_time_base);
            packet.write_interleaved(output)?;
        }
        Ok(())
    }
}

fn video_encoder_names(kind: VideoCodec) -> &'static [&'static str] {
    match kind {
        VideoCodec::H264 if cfg!(target_os = "macos") => &["h264_videotoolbox", "libx264", "h264"],
        VideoCodec::H264 => &["h264_nvenc", "libx264", "h264"],
        VideoCodec::H265 if cfg!(target_os = "macos") => &["hevc_videotoolbox", "libx265", "hevc"],
        VideoCodec::H265 => &["hevc_nvenc", "libx265", "hevc"],
        VideoCodec::ProRes4444 if cfg!(target_os = "macos") => {
            &["prores_videotoolbox", "prores_ks"]
        }
        VideoCodec::ProRes4444 => &["prores_ks"],
        VideoCodec::Vp9 => &["libvpx-vp9", "vp9"],
        VideoCodec::Gif => &["gif"],
        VideoCodec::Ffv1 => &["ffv1"],
    }
}

fn open_video_encoder(
    settings: &RenderSpec,
    target: [u32; 2],
    frame_rate: Rational,
    time_base: Rational,
    global: bool,
) -> Result<(ffmpeg::Codec, codec::encoder::video::Encoder, format::Pixel)> {
    let mut failures = Vec::new();
    for name in video_encoder_names(settings.preset.video_codec) {
        let Some(codec) = encoder::find_by_name(name) else {
            continue;
        };
        let pixel = choose_video_pixel_format(codec, settings.preset.video_codec)?;
        let mut video = codec::context::Context::new_with_codec(codec)
            .encoder()
            .video()?;
        video.set_width(target[0]);
        video.set_height(target[1]);
        video.set_format(pixel);
        video.set_frame_rate(Some(frame_rate));
        video.set_time_base(time_base);
        video.set_gop(u32::try_from(frame_rate.0.max(1)).unwrap_or(60).min(240) * 2);
        if global {
            video.set_flags(codec::Flags::GLOBAL_HEADER);
        }
        match video.open_as_with(codec, video_options(settings)) {
            Ok(encoder) => return Ok((codec, encoder, pixel)),
            Err(error) => failures.push(format!("{name}: {error}")),
        }
    }
    anyhow::bail!("no usable video encoder: {}", failures.join("; "))
}
fn audio_codec(kind: AudioCodec) -> Result<ffmpeg::Codec> {
    let names: &[&str] = match kind {
        AudioCodec::Aac => &["aac"],
        AudioCodec::Opus => &["libopus", "opus"],
        AudioCodec::Flac => &["flac"],
        AudioCodec::Pcm => &["pcm_s24le"],
    };
    names
        .iter()
        .find_map(|name| encoder::find_by_name(name))
        .context("requested audio encoder unavailable")
}
fn video_pixel_format(kind: VideoCodec) -> format::Pixel {
    match kind {
        VideoCodec::H264 => format::Pixel::YUV420P,
        VideoCodec::H265 => format::Pixel::YUV420P10LE,
        VideoCodec::ProRes4444 => format::Pixel::YUVA444P10LE,
        VideoCodec::Vp9 => format::Pixel::YUVA420P,
        VideoCodec::Gif => format::Pixel::RGB8,
        VideoCodec::Ffv1 => format::Pixel::GBRAP16LE,
    }
}
fn choose_video_pixel_format(codec: ffmpeg::Codec, kind: VideoCodec) -> Result<format::Pixel> {
    let preferred = video_pixel_format(kind);
    let formats = codec
        .video()?
        .formats()
        .context("video encoder exposes no pixel formats")?
        .collect::<Vec<_>>();
    if formats.contains(&preferred) {
        Ok(preferred)
    } else {
        formats
            .first()
            .copied()
            .context("video encoder exposes no pixel formats")
    }
}

fn video_options(settings: &RenderSpec) -> Dictionary<'static> {
    let mut options = Dictionary::new();
    match settings.preset.video_codec {
        VideoCodec::H264 | VideoCodec::H265 => {
            options.set("preset", "veryfast");
            options.set("crf", &settings.preset.quality.clamp(0, 51).to_string());
        }
        VideoCodec::ProRes4444 => {
            options.set("profile", "4");
            options.set("alpha_bits", "16");
        }
        VideoCodec::Vp9 => {
            options.set("crf", &settings.preset.quality.clamp(0, 51).to_string());
            options.set("b", "0");
            options.set("row-mt", "1");
        }
        VideoCodec::Ffv1 => {
            options.set("level", "3");
        }
        VideoCodec::Gif => {}
    }
    options
}

#[cfg(test)]
mod tests {
    use std::{
        io::Write,
        sync::{atomic::AtomicBool, Arc},
    };

    use super::*;
    use crate::render::{cache_encoder::CacheEncoder, RenderPreset, RenderResolution};

    #[test]
    fn transcodes_cache() {
        if encoder::find_by_name("libx264").is_none() {
            return;
        }
        let root =
            std::env::temp_dir().join(format!("kama-transcoder-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let cache = root.join("cache.mov");
        let output = root.join("output.mp4");
        let mut writer = CacheEncoder::new(&cache, [16, 16], 24.0, 1).unwrap();
        let mut source_frame = vec![0; 16 * 16 * 8];
        for alpha in source_frame[16 * 16 * 6..].chunks_exact_mut(2) {
            alpha.copy_from_slice(&1023u16.to_le_bytes());
        }
        for _ in 0..7 {
            writer.write_all(&source_frame).unwrap();
        }
        writer.finish().unwrap();
        let settings = RenderSpec {
            preset: RenderPreset {
                name: String::new(),
                category: String::new(),
                container: "mp4".into(),
                video_codec: VideoCodec::H264,
                resolution: RenderResolution::Canvas,
                quality: 28,
                include_audio: false,
                audio_codec: AudioCodec::Aac,
                audio_bitrate_kbps: 192,
                sample_rate: 48_000,
            },
            output: output.clone(),
            overwrite: true,
            begin_frame: 0,
            end_frame: 6,
            background: false,
            transcode: true,
        };
        transcode(
            &[cache],
            None,
            &output,
            &settings,
            [16, 16],
            24.0,
            &Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        let input = format::input(&output).unwrap();
        assert_eq!(input.stream(0).unwrap().frames(), 7);
        drop(input);
        std::fs::remove_dir_all(root).unwrap();
    }
}
