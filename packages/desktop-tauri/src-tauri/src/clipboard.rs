// The clipboard's text for the page (`mt::clipboard::read-text` and
// `write-text`, Electron's clipboard.readText and writeText), and images
// (`mt::clipboard::write-image`). The webviews
// refuse the page's own clipboard calls (WebKitGTK answers both with
// NotAllowedError), which left Paste as Plain Text with nothing to paste and
// Copy Path copying nothing.

// WebKitGTK 2.54 crashes (WebKit::Clipboard::formats) when it pastes while the
// clipboard offers no formats at all: nothing has been copied, the program
// that copied has quit, or the copy of an empty selection left the clipboard
// empty. Its pastes go through here, so they only happen when there is
// something to paste. Runs on the main thread.
#[cfg(target_os = "linux")]
pub fn when_pasteable(paste: impl FnOnce() + 'static) {
    let targets = gtk::gdk::Atom::intern("TARGETS");
    gtk::Clipboard::get(&gtk::gdk::SELECTION_CLIPBOARD).request_contents(&targets, move |_, data| {
        if data.targets().is_some_and(|offered| !offered.is_empty()) {
            paste();
        }
    });
}

#[tauri::command]
pub async fn clipboard_read_text(window: tauri::WebviewWindow) -> Result<String, String> {
    read_text(window).await
}

#[tauri::command]
pub async fn clipboard_write_text(window: tauri::WebviewWindow, text: String) -> Result<(), String> {
    write_text(window, text).await
}

#[cfg(target_os = "linux")]
async fn write_text(window: tauri::WebviewWindow, text: String) -> Result<(), String> {
    window
        .run_on_main_thread(move || gtk::Clipboard::get(&gtk::gdk::SELECTION_CLIPBOARD).set_text(&text))
        .map_err(|err| err.to_string())
}

#[cfg(windows)]
async fn write_text(_window: tauri::WebviewWindow, text: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || clipboard_win::set_clipboard_string(&text).map_err(|err| err.to_string()))
        .await
        .map_err(|err| err.to_string())?
}

#[cfg(not(any(target_os = "linux", windows)))]
async fn write_text(_window: tauri::WebviewWindow, _text: String) -> Result<(), String> {
    Err("writing the clipboard is not supported here".into())
}

// GTK's clipboard, the one the webview's own Paste reads. It belongs to the
// main thread and answers through a callback.
#[cfg(target_os = "linux")]
async fn read_text(window: tauri::WebviewWindow) -> Result<String, String> {
    use std::time::Duration;

    let (sender, receiver) = std::sync::mpsc::channel();
    window
        .run_on_main_thread(move || {
            gtk::Clipboard::get(&gtk::gdk::SELECTION_CLIPBOARD).request_text(move |_, text| {
                let _ = sender.send(text.unwrap_or_default().to_string());
            });
        })
        .map_err(|err| err.to_string())?;
    tauri::async_runtime::spawn_blocking(move || receiver.recv_timeout(Duration::from_secs(5)))
        .await
        .map_err(|err| err.to_string())?
        .map_err(|_| "the clipboard did not answer".to_string())
}

// Opening the clipboard is retried while another program holds it.
#[cfg(windows)]
async fn read_text(_window: tauri::WebviewWindow) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(|| {
        if !clipboard_win::is_format_avail(clipboard_win::formats::CF_UNICODETEXT) {
            return Ok(String::new());
        }
        clipboard_win::get_clipboard_string().map_err(|err| err.to_string())
    })
    .await
    .map_err(|err| err.to_string())?
}

#[cfg(not(any(target_os = "linux", windows)))]
async fn read_text(_window: tauri::WebviewWindow) -> Result<String, String> {
    Err("reading the clipboard is not supported here".into())
}

// Electron's clipboard.writeImage for `mt::clipboard::write-image`: false when
// the bytes are not an image or the clipboard cannot be written.
#[tauri::command]
pub async fn clipboard_write_image(window: tauri::WebviewWindow, png: Vec<u8>) -> bool {
    write_image(window, png).await.unwrap_or_else(|err| {
        eprintln!("[clipboard] writing an image failed: {err}");
        false
    })
}

// GdkPixbuf reads the bytes, so any format it knows is accepted, as Electron's
// nativeImage accepts JPEG too.
#[cfg(target_os = "linux")]
async fn write_image(window: tauri::WebviewWindow, png: Vec<u8>) -> Result<bool, String> {
    use std::time::Duration;

    use gtk::gdk_pixbuf::prelude::PixbufLoaderExt;

    let (sender, receiver) = std::sync::mpsc::channel();
    window
        .run_on_main_thread(move || {
            let loader = gtk::gdk_pixbuf::PixbufLoader::new();
            // Closed even after a failed write, or GdkPixbuf warns.
            let loaded = loader.write(&png).and(loader.close());
            let pixbuf = loaded.ok().and_then(|_| loader.pixbuf());
            if let Some(pixbuf) = &pixbuf {
                gtk::Clipboard::get(&gtk::gdk::SELECTION_CLIPBOARD).set_image(pixbuf);
            }
            let _ = sender.send(pixbuf.is_some());
        })
        .map_err(|err| err.to_string())?;
    tauri::async_runtime::spawn_blocking(move || receiver.recv_timeout(Duration::from_secs(5)))
        .await
        .map_err(|err| err.to_string())?
        .map_err(|_| "the clipboard did not answer".to_string())
}

// The PNG itself under the registered "PNG" format, which browsers, Office and
// most image editors paste with its transparency, and the decoded pixels as
// CF_DIBV5 and CF_DIB for programs that only take a bitmap (Paint, WordPad).
#[cfg(windows)]
async fn write_image(_window: tauri::WebviewWindow, png: Vec<u8>) -> Result<bool, String> {
    use clipboard_win::formats::{CF_DIB, CF_DIBV5};
    use clipboard_win::raw::set_without_clear;

    tauri::async_runtime::spawn_blocking(move || {
        let Some(bitmaps) = bitmaps_from_png(&png) else { return Ok(false) };
        let png_format = clipboard_win::register_format("PNG").ok_or("the PNG clipboard format is not available")?;
        let _clipboard = clipboard_win::Clipboard::new_attempts(10).map_err(|err| err.to_string())?;
        clipboard_win::empty().map_err(|err| err.to_string())?;
        set_without_clear(png_format.get(), &png)
            .and_then(|_| set_without_clear(CF_DIBV5, &bitmaps.dibv5))
            .and_then(|_| set_without_clear(CF_DIB, &bitmaps.dib))
            .map_err(|err| err.to_string())?;
        Ok(true)
    })
    .await
    .map_err(|err| err.to_string())?
}

#[cfg(not(any(target_os = "linux", windows)))]
async fn write_image(_window: tauri::WebviewWindow, _png: Vec<u8>) -> Result<bool, String> {
    Err("writing the clipboard is not supported here".into())
}

// The png crate's own limit (64 MiB) would refuse a large screenshot.
#[cfg(any(windows, test))]
const MAX_DECODED_BYTES: usize = 1 << 30;

// One decoded image as the two device independent bitmaps the clipboard
// takes: a header, then 32 bit pixels in blue, green, red, alpha order, rows
// from the bottom up. The alpha is not premultiplied.
#[cfg(any(windows, test))]
struct Bitmaps {
    // BITMAPINFOHEADER, BI_RGB: the alpha sits in the byte Windows calls reserved.
    dib: Vec<u8>,
    // BITMAPV5HEADER, BI_BITFIELDS with an alpha mask, sRGB.
    dibv5: Vec<u8>,
}

#[cfg(any(windows, test))]
fn bitmaps_from_png(png: &[u8]) -> Option<Bitmaps> {
    let mut decoder = png::Decoder::new_with_limits(std::io::Cursor::new(png), png::Limits { bytes: MAX_DECODED_BYTES });
    // Palettes, 16 bit samples and grey below 8 bits all become 8 bit samples.
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().ok()?;
    let mut decoded = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut decoded).ok()?;
    let width = i32::try_from(info.width).ok()?;
    let height = i32::try_from(info.height).ok()?;
    let samples = info.color_type.samples();
    let row_bytes = info.width as usize * samples;

    let mut pixels = Vec::with_capacity(info.width as usize * info.height as usize * 4);
    for row in decoded[..info.buffer_size()].chunks_exact(info.line_size).rev() {
        for pixel in row[..row_bytes].chunks_exact(samples) {
            let [red, green, blue, alpha] = match *pixel {
                [grey] => [grey, grey, grey, 255],
                [grey, alpha] => [grey, grey, grey, alpha],
                [red, green, blue] => [red, green, blue, 255],
                [red, green, blue, alpha] => [red, green, blue, alpha],
                _ => return None,
            };
            pixels.extend_from_slice(&[blue, green, red, alpha]);
        }
    }
    let image_size = u32::try_from(pixels.len()).ok()?;

    const BI_RGB: u32 = 0;
    const BI_BITFIELDS: u32 = 3;
    const LCS_SRGB: u32 = 0x7352_4742;
    const LCS_GM_IMAGES: u32 = 4;

    let mut dib = bitmap_info_header(40, width, height, BI_RGB, image_size);
    dib.extend_from_slice(&pixels);

    let mut dibv5 = bitmap_info_header(124, width, height, BI_BITFIELDS, image_size);
    for mask in [0x00ff_0000u32, 0x0000_ff00, 0x0000_00ff, 0xff00_0000, LCS_SRGB] {
        dibv5.extend_from_slice(&mask.to_le_bytes());
    }
    // Colour space end points and gamma, unused for sRGB.
    dibv5.extend_from_slice(&[0; 48]);
    // Rendering intent, then profile data, profile size and reserved.
    for value in [LCS_GM_IMAGES, 0, 0, 0] {
        dibv5.extend_from_slice(&value.to_le_bytes());
    }
    dibv5.extend_from_slice(&pixels);

    Some(Bitmaps { dib, dibv5 })
}

// The fields BITMAPINFOHEADER and BITMAPV5HEADER share. A positive height
// means the rows run from the bottom up.
#[cfg(any(windows, test))]
fn bitmap_info_header(header_size: u32, width: i32, height: i32, compression: u32, image_size: u32) -> Vec<u8> {
    let mut header = Vec::with_capacity(header_size as usize);
    header.extend_from_slice(&header_size.to_le_bytes());
    header.extend_from_slice(&width.to_le_bytes());
    header.extend_from_slice(&height.to_le_bytes());
    // One plane, 32 bits a pixel.
    header.extend_from_slice(&1u16.to_le_bytes());
    header.extend_from_slice(&32u16.to_le_bytes());
    header.extend_from_slice(&compression.to_le_bytes());
    header.extend_from_slice(&image_size.to_le_bytes());
    // Resolution and palette: none.
    header.extend_from_slice(&[0; 16]);
    header
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode(width: u32, height: u32, color: png::ColorType, depth: png::BitDepth, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(color);
        encoder.set_depth(depth);
        encoder.write_header().unwrap().write_image_data(data).unwrap();
        out
    }

    fn u32_at(bytes: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
    }

    #[test]
    fn rgba_becomes_bottom_up_bgra() {
        // Top row red then half transparent green, bottom row blue then white.
        let rgba = [255, 0, 0, 255, 0, 255, 0, 128, 0, 0, 255, 255, 255, 255, 255, 255];
        let png = encode(2, 2, png::ColorType::Rgba, png::BitDepth::Eight, &rgba);
        let Bitmaps { dib, dibv5 } = bitmaps_from_png(&png).unwrap();
        let pixels = [255, 0, 0, 255, 255, 255, 255, 255, 0, 0, 255, 255, 0, 255, 0, 128];

        assert_eq!(dib.len(), 40 + 16);
        assert_eq!(&dib[40..], &pixels);
        assert_eq!(u32_at(&dib, 0), 40);
        assert_eq!(u32_at(&dib, 4), 2);
        assert_eq!(u32_at(&dib, 8), 2);
        assert_eq!(u32_at(&dib, 12), 1 | (32 << 16));
        assert_eq!(u32_at(&dib, 16), 0);
        assert_eq!(u32_at(&dib, 20), 16);

        assert_eq!(dibv5.len(), 124 + 16);
        assert_eq!(&dibv5[124..], &pixels);
        assert_eq!(u32_at(&dibv5, 0), 124);
        assert_eq!(u32_at(&dibv5, 16), 3);
        assert_eq!(u32_at(&dibv5, 40), 0x00ff_0000);
        assert_eq!(u32_at(&dibv5, 52), 0xff00_0000);
        assert_eq!(u32_at(&dibv5, 56), 0x7352_4742);
        assert_eq!(u32_at(&dibv5, 108), 4);
    }

    #[test]
    fn other_colour_types_are_expanded() {
        let grey = encode(1, 1, png::ColorType::Grayscale, png::BitDepth::Sixteen, &[0x80, 0x00]);
        assert_eq!(&bitmaps_from_png(&grey).unwrap().dib[40..], &[0x80, 0x80, 0x80, 255]);

        let rgb = encode(1, 1, png::ColorType::Rgb, png::BitDepth::Eight, &[1, 2, 3]);
        assert_eq!(&bitmaps_from_png(&rgb).unwrap().dib[40..], &[3, 2, 1, 255]);

        let mut out = Vec::new();
        let mut encoder = png::Encoder::new(&mut out, 3, 1);
        encoder.set_color(png::ColorType::Indexed);
        encoder.set_depth(png::BitDepth::Two);
        encoder.set_palette(vec![10, 20, 30, 40, 50, 60]);
        encoder.set_trns(vec![255, 0]);
        encoder.write_header().unwrap().write_image_data(&[0b0001_0000]).unwrap();
        let indexed = bitmaps_from_png(&out).unwrap();
        assert_eq!(&indexed.dib[40..], &[30, 20, 10, 255, 60, 50, 40, 0, 30, 20, 10, 255]);
    }

    #[test]
    fn bytes_that_are_not_a_png_are_refused() {
        assert!(bitmaps_from_png(b"").is_none());
        assert!(bitmaps_from_png(b"not an image").is_none());
        let png = encode(2, 1, png::ColorType::Rgb, png::BitDepth::Eight, &[0; 6]);
        assert!(bitmaps_from_png(&png[..png.len() / 2]).is_none());
    }
}
