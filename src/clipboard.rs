//! Reads an image (e.g. a screenshot taken with Win+Shift+S) from the system
//! clipboard and prepares it for the Claude API.

use std::io::Cursor;

use base64::Engine;

/// Longest side sent to the API: larger screenshots are downscaled.
const MAX_SIDE: u32 = 1920;
/// API limit for a base64 image.
const MAX_BASE64_BYTES: usize = 5 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct Attachment {
    /// Base64-encoded PNG.
    pub png_base64: String,
    /// Thumbnail shown in the chat.
    pub handle: iced::widget::image::Handle,
    pub width: u32,
    pub height: u32,
}

/// Reads the clipboard image off the UI thread.
pub async fn read_image() -> Result<Attachment, String> {
    tokio::task::spawn_blocking(read_image_blocking)
        .await
        .map_err(|e| format!("lecture du presse-papiers interrompue : {e}"))?
}

fn read_image_blocking() -> Result<Attachment, String> {
    let mut clipboard = arboard::Clipboard::new().map_err(|e| format!("presse-papiers inaccessible : {e}"))?;
    let data = clipboard
        .get_image()
        .map_err(|_| "Aucune image dans le presse-papiers".to_string())?;

    let mut pixels = data.bytes.into_owned();
    // Screenshots are opaque, but Windows bitmaps often carry a zero alpha channel.
    for px in pixels.chunks_exact_mut(4) {
        px[3] = 255;
    }
    let mut img = image::RgbaImage::from_raw(data.width as u32, data.height as u32, pixels)
        .ok_or("image du presse-papiers invalide")?;

    let (w, h) = img.dimensions();
    if w.max(h) > MAX_SIDE {
        let scale = MAX_SIDE as f64 / w.max(h) as f64;
        let (nw, nh) = (((w as f64) * scale).round().max(1.0) as u32, ((h as f64) * scale).round().max(1.0) as u32);
        img = image::imageops::resize(&img, nw, nh, image::imageops::FilterType::Lanczos3);
    }
    let (width, height) = img.dimensions();

    let mut png = Vec::new();
    image::DynamicImage::ImageRgb8(image::DynamicImage::ImageRgba8(img.clone()).to_rgb8())
        .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|e| format!("encodage PNG impossible : {e}"))?;
    let png_base64 = base64::engine::general_purpose::STANDARD.encode(&png);
    if png_base64.len() > MAX_BASE64_BYTES {
        return Err("Image trop lourde (> 5 Mo)".into());
    }

    Ok(Attachment {
        png_base64,
        handle: iced::widget::image::Handle::from_rgba(width, height, img.into_raw()),
        width,
        height,
    })
}
