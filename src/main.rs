use color_eyre::Result;
use image::{ImageBuffer, Rgb};
use ratatui::{
    DefaultTerminal,
    crossterm::event::{self, Event, KeyCode},
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use rayon::prelude::*;
use std::{env, time::Duration};
use v4l::io::traits::CaptureStream;
use v4l::prelude::*;
use v4l::video::Capture;
use v4l::{buffer::Type, io::traits::Stream};

use crate::{
    filters::Filter,
    render::{RenderMode, YuvLut, blend, sample_bilinear, to_ascii},
};

mod filters;
mod render;

fn main() -> Result<()> {
    // Default device
    let mut device_path = "/dev/video0".to_string();
    // Parse CLI arguments
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--device" {
            if let Some(val) = args.next() {
                device_path = val;
            } else {
                eprintln!(
                    "Warning: --device provided but no path, using default {}",
                    device_path
                );
            }
        }
    }

    let dev = Device::with_path(device_path).expect("Failed to open device");
    let (cam_w, cam_h) = init(&dev);
    let mut stream = UserptrStream::with_buffers(&dev, Type::VideoCapture, 4)
        .expect("Failed to create UserptrStream");
    //let mut stream =
    //    MmapStream::with_buffers(&dev, Type::VideoCapture, 4).expect("Failed to create MmapStream");

    let lut = YuvLut::build();

    let mut frame = vec![0u32; (cam_w * cam_h) as usize];
    let mut prev_frame = vec![0u32; (cam_w * cam_h) as usize];

    color_eyre::install()?;
    let mut terminal = ratatui::init();

    // Pre-allocate render buffers, reused every frame
    let mut lines: Vec<Line> = Vec::with_capacity(256);

    // user config
    let mut filter = Filter::Normal;
    let mut mode = RenderMode::HalfBlock;
    let mut is_sample_bilinear = false;
    let mut pause = false;
    let mut screenshot = false;
    let mut quit = false;

    while !quit {
        let active_filter = filter;
        let active_mode = mode;

        if !pause {
            if let Some(buf) = next_frame_safe(&mut stream)? {
                // Parallel YUYV -> packed RGB decode across all pixel pairs simultaneously
                frame
                    .par_chunks_mut(2)
                    .zip(buf.par_chunks_exact(4_usize))
                    .for_each(|(out, chunk)| {
                        let y0 = chunk[0];
                        let u = chunk[1];
                        let y1 = chunk[2];
                        let v = chunk[3];
                        out[0] = lut.lookup(y0, u, v);
                        out[1] = lut.lookup(y1, u, v);
                    });
                if is_sample_bilinear {
                    for i in 0..frame.len() {
                        frame[i] = blend(prev_frame[i], frame[i], 0.6);
                    }
                    prev_frame.copy_from_slice(&frame);
                }
            } else {
                // reinit stream
                drop(stream);
                stream = UserptrStream::with_buffers(&dev, Type::VideoCapture, 4)
                    .expect("Failed to create UserptrStream");
            }
        }

        terminal.draw(|f| {
            let size = f.area();
            let term_w = size.width as usize;
            let term_h = size.height as usize;
            let cam_hu = cam_h as usize;
            let cam_wu = cam_w as usize;

            let scale_x = cam_wu as f32 / term_w.max(1) as f32;

            let new_lines: Vec<Line> = match active_mode {
                // Half-block mode
                RenderMode::HalfBlock => {
                    let scale_y = cam_hu as f32 / (term_h * 2).max(1) as f32;

                    (0..term_h)
                        .into_par_iter()
                        .map(|ty| {
                            let spans: Vec<Span> = (0..term_w)
                                .map(|tx| {
                                    let fx = tx as f32 * scale_x;
                                    let fy1 = (ty * 2) as f32 * scale_y;
                                    let fy2 = (ty * 2 + 1) as f32 * scale_y;
                                    let (p1, p2) = if is_sample_bilinear {
                                        (
                                            sample_bilinear(&frame, cam_wu, cam_hu, fx, fy1),
                                            sample_bilinear(&frame, cam_wu, cam_hu, fx, fy2),
                                        )
                                    } else {
                                        // Nearest-neighbor: cast to usize and clamp to frame bounds
                                        let cx = fx as usize;
                                        let cy1 = fy1 as usize;
                                        let cy2 = fy2 as usize;

                                        let cx = cx.min(cam_wu - 1);
                                        let cy1 = cy1.min(cam_hu - 1);
                                        let cy2 = cy2.min(cam_hu - 1);

                                        (frame[cy1 * cam_wu + cx], frame[cy2 * cam_wu + cx])
                                    };
                                    let p1 = active_filter.apply(p1, ty * 2);
                                    let p2 = active_filter.apply(p2, ty * 2 + 1);
                                    Span::styled(
                                        "▀",
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

                // ASCII mode
                // One camera pixel → one terminal cell.
                // Filters still apply; luma drives the ramp character;
                // original (filtered) colour tints the character.
                RenderMode::Ascii => {
                    let scale_y = cam_hu as f32 / term_h.max(1) as f32;

                    (0..term_h)
                        .into_par_iter()
                        .map(|ty| {
                            let spans: Vec<Span> = (0..term_w)
                                .map(|tx| {
                                    let fx = tx as f32 * scale_x;
                                    let fy = ty as f32 * scale_y;
                                    let px = if is_sample_bilinear {
                                        sample_bilinear(&frame, cam_wu, cam_hu, fx, fy)
                                    } else {
                                        let ix = (fx as usize).min(cam_wu - 1);
                                        let iy = (fy as usize).min(cam_hu - 1);
                                        frame[iy * cam_wu + ix]
                                    };

                                    let px = active_filter.apply(px, ty);
                                    let (ch, fg) = to_ascii(px);
                                    Span::styled(
                                        // We need an owned String for each unique char
                                        ch.to_string(),
                                        Style::default().fg(fg),
                                    )
                                })
                                .collect();
                            Line::from(spans)
                        })
                        .collect()
                }
            };

            lines.clear();
            lines.extend(new_lines);
            if screenshot {
                save_frame_ansi(&lines).expect("Failed to save screenshot ansi");
                save_png(&frame, cam_wu, cam_hu).expect("Failed to save screensho png");
                screenshot = false;
            }
            // Overlay: filter name in top-left corner, one Paragraph on top of another
            let label = format!(
                " {} │ {} │ AA:{} │ {} │ q quit ",
                active_mode.label(),
                active_filter.name(),
                is_sample_bilinear,
                if pause { "PAUSED" } else { "LIVE" }
            );
            let label_w = label.len() as u16;

            use ratatui::layout::Rect;
            use ratatui::widgets::Clear;
            f.render_widget(Paragraph::new(lines.clone()), size);
            let badge = Rect {
                x: 1,
                y: 0,
                width: label_w,
                height: 1,
            };
            f.render_widget(Clear, badge);
            f.render_widget(
                Paragraph::new(label.as_str())
                    .style(Style::default().fg(Color::Green).bg(Color::Black)),
                badge,
            );
        })?;

        while event::poll(Duration::from_millis(0))? {
            match handle_input(
                &mut terminal,
                filter,
                mode,
                is_sample_bilinear,
                pause,
                screenshot,
            )? {
                (true, _, _, _, _, _) => {
                    quit = true;
                    break; // quit
                }
                (false, next_filter, next_mode, next_sample, next_pause, next_screenshot) => {
                    filter = next_filter;
                    mode = next_mode;
                    is_sample_bilinear = next_sample;
                    pause = next_pause;
                    screenshot = next_screenshot;
                }
            }
        }
    }

    ratatui::restore();
    Ok(())
}

fn init(dev: &Device) -> (u32, u32) {
    let mut format = Capture::format(dev).expect("Failed to get format");
    format.fourcc = v4l::FourCC::new(b"YUYV");
    let format = Capture::set_format(dev, &format).expect("Failed to set format");
    (format.width, format.height)
}

// TODO refactor
fn handle_input(
    terminal: &mut DefaultTerminal,
    filter: Filter,
    mode: RenderMode,
    sample_bilinear: bool,
    pause: bool,
    screenshot: bool,
) -> Result<(bool, Filter, RenderMode, bool, bool, bool)> {
    match event::read()? {
        Event::Key(key) => {
            match key.code {
                KeyCode::Char('q') => {
                    return Ok((true, filter, mode, sample_bilinear, pause, screenshot));
                }
                KeyCode::Char('a') => {
                    let next = match mode {
                        RenderMode::HalfBlock => RenderMode::Ascii,
                        RenderMode::Ascii => RenderMode::HalfBlock,
                    };
                    return Ok((false, filter, next, sample_bilinear, pause, screenshot));
                }
                KeyCode::Char('s') => {
                    // Toggle sample_bilinear
                    return Ok((false, filter, mode, !sample_bilinear, pause, screenshot));
                }
                KeyCode::Char('p') => {
                    return Ok((false, filter, mode, sample_bilinear, pause, !screenshot));
                }
                KeyCode::Char(' ') => {
                    // Toggle pause
                    return Ok((false, filter, mode, sample_bilinear, !pause, screenshot));
                }
                KeyCode::Char(c) => {
                    if let Some(f) = Filter::from_key(c) {
                        return Ok((false, f, mode, sample_bilinear, pause, screenshot));
                    }
                }
                _ => {}
            }
        }
        Event::Resize(_w, _h) => terminal.autoresize()?, // works via mutable borrow
        _ => (),
    }
    return Ok((false, filter, mode, sample_bilinear, pause, screenshot));
}

/// Works for any V4L stream implementing CaptureStream (MmapStream, UserptrStream, etc.)
fn next_frame_safe<'a, S>(stream: &'a mut S) -> Result<Option<Vec<u8>>>
where
    S: CaptureStream<'a>,
    S::Item: AsRef<[u8]>,
{
    match stream.next() {
        Ok((buf, _meta)) => Ok(Some(buf.as_ref().to_vec())), // convert to Vec<u8>
        Err(e) => match e.raw_os_error() {
            Some(4) => Ok(None), // EINTR
            Some(22) => {
                // EINVAL
                eprintln!("Warning: frame skipped due to EINVAL (terminal resize)");
                Ok(None)
            }
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
        for span in line.spans.iter() {
            let style = span.style;

            if let Some(fg) = style.fg {
                if let Color::Rgb(r, g, b) = fg {
                    write!(file, "\x1b[38;2;{};{};{}m", r, g, b)?;
                }
            }

            if let Some(bg) = style.bg {
                if let Color::Rgb(r, g, b) = bg {
                    write!(file, "\x1b[48;2;{};{};{}m", r, g, b)?;
                }
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
