use clap::Parser;
use color_eyre::Result;
use color_eyre::eyre::WrapErr;
use image::{ImageBuffer, Rgb};
use ratatui::{
    DefaultTerminal,
    crossterm::event::{self, Event, KeyCode},
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};
use rayon::prelude::*;
use std::path::PathBuf;
use std::time::Duration;
use v4l::buffer::Type;
use v4l::io::mmap::Stream;
use v4l::io::traits::CaptureStream;
use v4l::prelude::*;
use v4l::video::Capture;

use pixels::{
    filters::Filter,
    matrix::MatrixState,
    render::{RenderMode, YuvLut, blend, sample_bilinear, to_ascii},
};

const PIXEL: &str = "▀";

struct App<'a> {
    stream: Option<MmapStream<'a>>,
    cam_w: u32,
    cam_h: u32,
    lut: YuvLut,
    frame: Vec<u32>,
    prev_frame: Vec<u32>,
    filter: Filter,
    mode: RenderMode,
    is_sample_bilinear: bool,
    pause: bool,
    show_overlay: bool,
    show_help: bool,
    screenshot: bool,
    quit: bool,
    // Pre-allocated strings for ASCII mode to avoid per-frame allocations
    ascii_strings: Vec<String>,
    consecutive_skips: u32,

    matrix: MatrixState,
}

impl<'a> App<'a> {
    fn new(stream: Stream<'a>, cam_w: u32, cam_h: u32, term_w: u16, term_h: u16) -> Self {
        let ascii_strings = (0..=255u8).map(|c| (c as char).to_string()).collect();
        let matrix = MatrixState::new(term_w, term_h);

        Self {
            stream: Some(stream),
            cam_w,
            cam_h,
            lut: YuvLut::build(),
            frame: vec![0u32; (cam_w * cam_h) as usize],
            prev_frame: vec![0u32; (cam_w * cam_h) as usize],
            filter: Filter::Normal,
            mode: RenderMode::HalfBlock,
            is_sample_bilinear: false,
            pause: false,
            show_overlay: true,
            show_help: false,
            screenshot: false,
            quit: false,
            ascii_strings,
            consecutive_skips: 0,
            matrix,
        }
    }

    fn update(&mut self, dev: &Device) -> Result<()> {
        if self.pause {
            return Ok(());
        }

        // Only try to get a frame if we have a stream
        if let Some(stream) = self.stream.as_mut() {
            match next_frame_safe(stream) {
                Ok(Some(buf)) => {
                    self.consecutive_skips = 0;
                    let frame = &mut self.frame;
                    let lut = &self.lut;

                    // Parallel YUYV -> packed RGB decode
                    frame
                        .par_chunks_mut(2)
                        .zip(buf.par_chunks_exact(4_usize))
                        .for_each(|(out, chunk)| {
                            out[0] = lut.lookup(chunk[0], chunk[1], chunk[3]);
                            out[1] = lut.lookup(chunk[2], chunk[1], chunk[3]);
                        });

                    if self.is_sample_bilinear {
                        frame
                            .par_iter_mut()
                            .zip(self.prev_frame.par_iter())
                            .for_each(|(curr, prev)| {
                                *curr = blend(*prev, *curr, 0.6);
                            });
                        self.prev_frame.copy_from_slice(frame);
                    }
                }
                Ok(None) => {
                    // Transient error (e.g. resize), just skip this frame
                    self.consecutive_skips += 1;
                    if self.consecutive_skips > 30 {
                        // Re-init after too many skips
                        self.stream.take(); // Ensure old stream is dropped
                        self.stream = Some(MmapStream::new(dev, Type::VideoCapture)?);
                        self.consecutive_skips = 0;
                    }
                }
                Err(_) => {
                    // Hard error, reinit stream
                    self.stream.take(); // Ensure old stream is dropped
                    self.stream = Some(MmapStream::new(dev, Type::VideoCapture)?);
                    self.consecutive_skips = 0;
                }
            }
        } else {
            // No stream? Try to create one
            self.stream.take(); // Ensure old stream is dropped
            self.stream = Some(MmapStream::new(dev, Type::VideoCapture)?);
            self.consecutive_skips = 0;
        }
        Ok(())
    }

    fn draw(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        terminal.draw(|f| {
            let area = f.area();
            if area.width == 0 || area.height == 0 {
                return;
            }

            let term_w = area.width as usize;
            let term_h = area.height as usize;
            let cam_w = self.cam_w as usize;
            let cam_h = self.cam_h as usize;

            let scale_x = cam_w as f32 / term_w as f32;

            let matrix = &mut self.matrix;

            let lines: Vec<Line> = match self.mode {
                RenderMode::HalfBlock => {
                    let scale_y = cam_h as f32 / (term_h * 2) as f32;
                    (0..term_h)
                        .into_par_iter()
                        .map(|ty| {
                            let spans: Vec<Span> = (0..term_w)
                                .rev()
                                .map(|tx| {
                                    let fx = tx as f32 * scale_x;
                                    let fy1 = (ty * 2) as f32 * scale_y;
                                    let fy2 = (ty * 2 + 1) as f32 * scale_y;

                                    let (p1, p2) = if self.is_sample_bilinear {
                                        (
                                            sample_bilinear(&self.frame, cam_w, cam_h, fx, fy1),
                                            sample_bilinear(&self.frame, cam_w, cam_h, fx, fy2),
                                        )
                                    } else {
                                        let cx = (fx as usize).min(cam_w - 1);
                                        let cy1 = (fy1 as usize).min(cam_h - 1);
                                        let cy2 = (fy2 as usize).min(cam_h - 1);
                                        (self.frame[cy1 * cam_w + cx], self.frame[cy2 * cam_w + cx])
                                    };

                                    let p1 = self.filter.apply(p1, ty * 2);
                                    let p2 = self.filter.apply(p2, ty * 2 + 1);

                                    Span::styled(
                                        PIXEL,
                                        Style::default()
                                            .fg(Color::Rgb(
                                                ((p1 >> 16) & 0xFF) as u8,
                                                ((p1 >> 8) & 0xFF) as u8,
                                                (p1 & 0xFF) as u8,
                                            ))
                                            .bg(Color::Rgb(
                                                ((p2 >> 16) & 0xFF) as u8,
                                                ((p2 >> 8) & 0xFF) as u8,
                                                (p2 & 0xFF) as u8,
                                            )),
                                    )
                                })
                                .collect();
                            Line::from(spans)
                        })
                        .collect()
                }
                RenderMode::Ascii => {
                    let scale_y = cam_h as f32 / term_h as f32;
                    (0..term_h)
                        .into_par_iter()
                        .map(|ty| {
                            let spans: Vec<Span> = (0..term_w)
                                .rev()
                                .map(|tx| {
                                    let fx = tx as f32 * scale_x;
                                    let fy = ty as f32 * scale_y;
                                    let px = if self.is_sample_bilinear {
                                        sample_bilinear(&self.frame, cam_w, cam_h, fx, fy)
                                    } else {
                                        let ix = (fx as usize).min(cam_w - 1);
                                        let iy = (fy as usize).min(cam_h - 1);
                                        self.frame[iy * cam_w + ix]
                                    };

                                    let px = self.filter.apply(px, ty);
                                    let (ch, fg) = to_ascii(px);
                                    // Use pre-allocated string
                                    Span::styled(
                                        &self.ascii_strings[ch as usize],
                                        Style::default().fg(fg),
                                    )
                                })
                                .collect();
                            Line::from(spans)
                        })
                        .collect()
                }

                RenderMode::Matrix => {
                    matrix.render_lines(&self.frame, cam_w, cam_h, term_w, term_h, self.pause)
                }
            };

            if self.screenshot {
                save_frame_ansi(&lines).expect("Failed to save ANSI screenshot");
                save_png(&self.frame, cam_w, cam_h).expect("Failed to save PNG screenshot");
                self.screenshot = false;
            }

            f.render_widget(Paragraph::new(lines), area);

            if self.show_overlay {
                let label = format!(
                    " {} │ {} │ AA:{} │ {} │ 'h' help ",
                    self.mode.label(),
                    self.filter.name(),
                    if self.is_sample_bilinear { "ON" } else { "OFF" },
                    if self.pause { "PAUSED" } else { "LIVE" }
                );
                let badge = Rect {
                    x: 1,
                    y: 0,
                    width: label.len() as u16,
                    height: 1,
                };
                let badge = badge.intersection(area);
                f.render_widget(Clear, badge);
                f.render_widget(
                    Paragraph::new(label).style(Style::default().fg(Color::Green).bg(Color::Black)),
                    badge,
                );
            }

            if self.show_help {
                let help_text = vec![
                    Line::from(" Keyboard Controls "),
                    Line::from("-------------------"),
                    Line::from(" q      : Quit"),
                    Line::from(" a      : Toggle Render Mode (RGB/ASCII/Matrix)"),
                    Line::from(" s      : Toggle Bilinear Anti-aliasing"),
                    Line::from(" p      : Save Screenshot (ANSI & PNG)"),
                    Line::from(" Space  : Toggle Pause"),
                    Line::from(" c      : Toggle Overlay"),
                    Line::from(" h      : Toggle Help Menu"),
                    Line::from(" 0-9    : Change Filter"),
                ];
                let help_width = 40;
                let help_height = help_text.len() as u16 + 2;
                let help_area = Rect {
                    x: (area.width.saturating_sub(help_width)) / 2,
                    y: (area.height.saturating_sub(help_height)) / 2,
                    width: help_width,
                    height: help_height,
                };
                let help_area = help_area.intersection(area);
                f.render_widget(Clear, help_area);
                f.render_widget(
                    Paragraph::new(help_text)
                        .block(Block::default().borders(Borders::ALL).title(" Help "))
                        .style(Style::default().fg(Color::White).bg(Color::Black)),
                    help_area,
                );
            }
        })?;
        Ok(())
    }

    fn handle_events(&mut self) -> Result<()> {
        while event::poll(Duration::from_millis(0))? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('q') => self.quit = true,
                    KeyCode::Char('a') => {
                        self.mode = match self.mode {
                            RenderMode::HalfBlock => RenderMode::Ascii,
                            RenderMode::Ascii => RenderMode::Matrix,
                            RenderMode::Matrix => RenderMode::HalfBlock,
                        };
                    }
                    KeyCode::Char('s') => self.is_sample_bilinear = !self.is_sample_bilinear,
                    KeyCode::Char('p') => self.screenshot = true,
                    KeyCode::Char(' ') => self.pause = !self.pause,
                    KeyCode::Char('c') => self.show_overlay = !self.show_overlay,
                    KeyCode::Char('h') => self.show_help = !self.show_help,
                    KeyCode::Char(c) => {
                        if let Some(f) = Filter::from_key(c) {
                            self.filter = f;
                        }
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Parser)]
#[command(version, about, long_about = None)]
struct Args {
    #[clap(short, long)]
    device: PathBuf,
}

fn main() -> Result<()> {
    color_eyre::install()?;

    let args = Args::parse();

    let dev = Device::with_path(&args.device)
        .wrap_err_with(|| format!("Cannot open device: {}", args.device.display()))?;
    let (cam_w, cam_h) = init(&dev)?;

    let stream = MmapStream::with_buffers(&dev, Type::VideoCapture, 4).wrap_err_with(|| {
        format!(
            "Failed to create MmapStream for device: {}",
            args.device.display()
        )
    })?;

    let mut terminal = ratatui::init();
    let size = terminal.size()?;
    let mut app = App::new(stream, cam_w, cam_h, size.width, size.height);

    while !app.quit {
        app.update(&dev)?;
        app.draw(&mut terminal)?;
        app.handle_events()?;
    }

    ratatui::restore();
    Ok(())
}

fn init(dev: &Device) -> Result<(u32, u32)> {
    let mut format = Capture::format(dev).wrap_err("Failed to get format")?;
    format.fourcc = v4l::FourCC::new(b"YUYV");
    let format = Capture::set_format(dev, &format).wrap_err("Failed to set format")?;
    Ok((format.width, format.height))
}

fn next_frame_safe<'a, S>(stream: &'a mut S) -> Result<Option<Vec<u8>>>
where
    S: CaptureStream<'a>,
    S::Item: AsRef<[u8]>,
{
    match stream.next() {
        Ok((buf, _meta)) => Ok(Some(buf.as_ref().to_vec())),
        Err(e) => match e.raw_os_error() {
            Some(4) | Some(22) => Ok(None), // EINTR or EINVAL (resize)
            _ => Err(e.into()),
        },
    }
}

fn save_frame_ansi(lines: &[Line]) -> Result<()> {
    use std::fs::File;
    use std::io::Write;
    use std::time::{SystemTime, UNIX_EPOCH};

    let ts = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let filename = format!("frame_{ts}.ansi");
    let mut file = File::create(&filename)?;

    for line in lines {
        for span in &line.spans {
            let style = span.style;
            if let Some(Color::Rgb(r, g, b)) = style.fg {
                write!(file, "\x1b[38;2;{};{};{}m", r, g, b)?;
            }
            if let Some(Color::Rgb(r, g, b)) = style.bg {
                write!(file, "\x1b[48;2;{};{};{}m", r, g, b)?;
            }
            write!(file, "{}", span.content)?;
            write!(file, "\x1b[0m")?;
        }
        writeln!(file)?;
    }
    Ok(())
}

fn save_png(frame: &[u32], width: usize, height: usize) -> Result<()> {
    let mut img = ImageBuffer::<Rgb<u8>, Vec<u8>>::new(width as u32, height as u32);
    for (i, pixel) in frame.iter().enumerate() {
        let x = (i % width) as u32;
        let y = (i / width) as u32;
        let r = ((pixel >> 16) & 0xFF) as u8;
        let g = ((pixel >> 8) & 0xFF) as u8;
        let b = (pixel & 0xFF) as u8;
        img.put_pixel(x, y, Rgb([r, g, b]));
    }

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    let filename = format!("frame_{ts}.png");
    img.save(&filename)?;
    Ok(())
}
