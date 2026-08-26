use std::{io, path::Path};

use anyhow::{Context, Result};
use ffmpeg::{codec, encoder, format, frame, Dictionary, Packet, Rational};
use ffmpeg_next as ffmpeg;

pub(super) struct CacheEncoder {
    output: format::context::Output,
    encoder: encoder::video::Encoder,
    stream_index: usize,
    encoder_time_base: Rational,
    stream_time_base: Rational,
    width: u32,
    height: u32,
    frame_bytes: usize,
    pending: Vec<u8>,
    next_pts: i64,
    finished: bool,
}

impl CacheEncoder {
    pub(super) fn new(path: &Path, size: [u32; 2], fps: f64, threads: usize) -> Result<Self> {
        crate::runtime::media::init_ffmpeg()?;
        let width = size[0].max(1);
        let height = size[1].max(1);
        let codec =
            encoder::find_by_name("prores_ks").context("FFmpeg prores_ks encoder unavailable")?;
        let mut output = format::output(path)
            .with_context(|| format!("create cache container {}", path.display()))?;
        let global_header = output
            .format()
            .flags()
            .contains(format::Flags::GLOBAL_HEADER);
        let fps_den = 1_000;
        let fps_num = (fps.max(1.0) * f64::from(fps_den)).round() as i32;
        let frame_rate = Rational(fps_num.max(1), fps_den);
        let time_base = Rational(fps_den, fps_num.max(1));

        let mut video = codec::context::Context::new_with_codec(codec)
            .encoder()
            .video()
            .context("create ProRes cache encoder")?;
        video.set_width(width);
        video.set_height(height);
        video.set_format(format::Pixel::YUVA444P10LE);
        video.set_frame_rate(Some(frame_rate));
        video.set_time_base(time_base);
        video.set_gop(1);
        if global_header {
            video.set_flags(codec::Flags::GLOBAL_HEADER);
        }

        let mut options = Dictionary::new();
        options.set("profile", "4");
        options.set("alpha_bits", "16");
        options.set("threads", &threads.max(1).to_string());
        let encoder = video
            .open_as_with(codec, options)
            .context("open FFmpeg ProRes cache encoder")?;
        let stream_index = {
            let mut stream = output
                .add_stream(codec)
                .context("add ProRes cache stream")?;
            stream.set_parameters(&encoder);
            stream.set_time_base(time_base);
            stream.set_rate(frame_rate);
            stream.index()
        };
        output.write_header().context("write ProRes cache header")?;
        let stream_time_base = output
            .stream(stream_index)
            .context("ProRes cache stream disappeared")?
            .time_base();
        let frame_bytes = usize::try_from(width)
            .ok()
            .and_then(|width| {
                usize::try_from(height)
                    .ok()
                    .map(|height| width * height * 8)
            })
            .context("cache frame size overflow")?;

        Ok(Self {
            output,
            encoder,
            stream_index,
            encoder_time_base: time_base,
            stream_time_base,
            width,
            height,
            frame_bytes,
            pending: Vec::with_capacity(frame_bytes),
            next_pts: 0,
            finished: false,
        })
    }

    fn encode_frame(&mut self, bytes: &[u8]) -> Result<()> {
        let row_bytes = self.width as usize * 2;
        let plane_bytes = row_bytes * self.height as usize;
        anyhow::ensure!(
            bytes.len() == plane_bytes * 4,
            "invalid cache frame byte count"
        );
        let mut video = frame::Video::new(format::Pixel::YUVA444P10LE, self.width, self.height);
        for plane in 0..4 {
            let stride = video.stride(plane);
            let target = video.data_mut(plane);
            let source = &bytes[plane * plane_bytes..(plane + 1) * plane_bytes];
            for row in 0..self.height as usize {
                target[row * stride..row * stride + row_bytes]
                    .copy_from_slice(&source[row * row_bytes..(row + 1) * row_bytes]);
            }
        }
        video.set_pts(Some(self.next_pts));
        self.next_pts += 1;
        self.encoder
            .send_frame(&video)
            .context("submit ProRes cache frame")?;
        self.drain_packets()
    }

    fn drain_packets(&mut self) -> Result<()> {
        let mut packet = Packet::empty();
        while self.encoder.receive_packet(&mut packet).is_ok() {
            packet.set_stream(self.stream_index);
            packet.set_duration(1);
            packet.rescale_ts(self.encoder_time_base, self.stream_time_base);
            packet
                .write_interleaved(&mut self.output)
                .context("write ProRes cache packet")?;
        }
        Ok(())
    }

    pub(super) fn finish(mut self) -> Result<()> {
        anyhow::ensure!(self.pending.is_empty(), "incomplete raw cache frame");
        self.encoder
            .send_eof()
            .context("flush ProRes cache encoder")?;
        self.drain_packets()?;
        self.output
            .write_trailer()
            .context("write ProRes cache trailer")?;
        self.finished = true;
        Ok(())
    }
}

impl io::Write for CacheEncoder {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.pending.extend_from_slice(bytes);
        while self.pending.len() >= self.frame_bytes {
            let remainder = self.pending.split_off(self.frame_bytes);
            let frame = std::mem::replace(&mut self.pending, remainder);
            self.encode_frame(&frame)
                .map_err(|error| io::Error::other(format!("{error:#}")))?;
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::CacheEncoder;

    #[test]
    fn writes_prores_without_cli() {
        let path = std::env::temp_dir().join(format!(
            "kama-cache-encoder-test-{}.mov",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let mut encoder = CacheEncoder::new(&path, [16, 16], 24.0, 1).unwrap();
        for _ in 0..7 {
            encoder.write_all(&vec![0; 16 * 16 * 8]).unwrap();
        }
        encoder.finish().unwrap();
        let input = ffmpeg_next::format::input(&path).unwrap();
        assert_eq!(input.streams().len(), 1);
        assert_eq!(input.stream(0).unwrap().frames(), 7);
        drop(input);
        std::fs::remove_file(path).unwrap();
    }
}
