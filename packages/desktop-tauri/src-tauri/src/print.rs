// Export to PDF and Print for the page (src/export.ts), in place of
// Electron's webContents.printToPDF and webContents.print
// (packages/desktop/src/main/menu/actions/file.ts). Both print the editor
// page itself, which the renderer has prepared with a print container and
// print styles. WebKitGTK ignores the margins of the page's CSS @page rule,
// so the page reads them out and they go on the paper; WebView2 gets them
// the same way, which gives the same page whether it honours the rule or not.
//
// The webviews answer through callbacks on the main thread; each command
// waits for the answer on a channel.

use serde::Deserialize;

// The export dialog's page options (packages/desktop/src/renderer/src/
// components/exportSettings/exportOptions.ts getPageSizeList), sizes in mm.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageOptions {
    page_size: Option<String>,
    page_size_width: Option<f64>,
    page_size_height: Option<f64>,
    is_landscape: Option<bool>,
}

// A sheet in portrait, in mm. `name` is GTK's name for the size, where the
// size has one.
// The page's margins, in mm.
#[derive(Deserialize)]
pub struct Margins {
    top: f64,
    right: f64,
    bottom: f64,
    left: f64,
}

#[derive(Debug, PartialEq)]
struct Paper {
    name: Option<&'static str>,
    width: f64,
    height: f64,
}

const MM_PER_INCH: f64 = 25.4;

// Chromium's sizes for the names Electron passes on to printToPDF. An unknown
// or incomplete size falls back to the dialog's default, A4.
fn paper(options: &PageOptions) -> Paper {
    let named = |name, width, height| Paper { name: Some(name), width, height };
    match options.page_size.as_deref() {
        Some("A3") => named("iso_a3", 297.0, 420.0),
        Some("A5") => named("iso_a5", 148.0, 210.0),
        Some("Legal") => named("na_legal", 8.5 * MM_PER_INCH, 14.0 * MM_PER_INCH),
        Some("Letter") => named("na_letter", 8.5 * MM_PER_INCH, 11.0 * MM_PER_INCH),
        Some("Tabloid") => named("na_ledger", 11.0 * MM_PER_INCH, 17.0 * MM_PER_INCH),
        Some("custom") => match (options.page_size_width, options.page_size_height) {
            (Some(width), Some(height)) if width > 0.0 && height > 0.0 => Paper { name: None, width, height },
            _ => named("iso_a4", 210.0, 297.0),
        },
        _ => named("iso_a4", 210.0, 297.0),
    }
}

#[cfg_attr(not(any(windows, test)), allow(dead_code))]
fn inches(mm: f64) -> f64 {
    mm / MM_PER_INCH
}

// Writes the page to `path` as a PDF, without a dialog.
#[tauri::command]
pub async fn print_to_pdf(
    window: tauri::WebviewWindow,
    path: String,
    options: PageOptions,
    margins: Margins,
) -> Result<(), String> {
    let (sender, mut receiver) = tauri::async_runtime::channel(1);
    start_pdf(&window, path, options, margins, sender).map_err(|err| err.to_string())?;
    receiver.recv().await.unwrap_or_else(|| Err("printing stopped without an answer".into()))
}

// Shows the system print dialog for the page. Returns once the dialog is
// closed and, when the user printed, the job has been handed over.
#[tauri::command]
pub async fn print_page(window: tauri::WebviewWindow, margins: Margins) -> Result<(), String> {
    let (sender, mut receiver) = tauri::async_runtime::channel(1);
    start_print(&window, margins, sender).map_err(|err| err.to_string())?;
    receiver.recv().await.unwrap_or_else(|| Err("printing stopped without an answer".into()))
}

type Done = tauri::async_runtime::Sender<Result<(), String>>;

#[cfg(target_os = "linux")]
fn set_margins(setup: &gtk::PageSetup, margins: &Margins) {
    use gtk::Unit;

    setup.set_top_margin(margins.top, Unit::Mm);
    setup.set_bottom_margin(margins.bottom, Unit::Mm);
    setup.set_left_margin(margins.left, Unit::Mm);
    setup.set_right_margin(margins.right, Unit::Mm);
}

// WebKit reports a failure and then the end of the operation; only the first
// answer counts.
#[cfg(target_os = "linux")]
fn watch_operation(operation: &webkit2gtk::PrintOperation, done: Done) {
    use std::cell::RefCell;
    use std::rc::Rc;

    use webkit2gtk::PrintOperationExt;

    // Holds the operation until it ends.
    let alive = Rc::new(RefCell::new(Some(operation.clone())));
    let failed_done = done.clone();
    operation.connect_failed(move |_, err| {
        eprintln!("[print] {err}");
        let _ = failed_done.try_send(Err(err.to_string()));
    });
    operation.connect_finished(move |_| {
        alive.borrow_mut().take();
        let _ = done.try_send(Ok(()));
    });
}

// GTK's "Print to File" printer, whose name GTK translates.
#[cfg(target_os = "linux")]
fn start_pdf(
    window: &tauri::WebviewWindow,
    path: String,
    options: PageOptions,
    margins: Margins,
    done: Done,
) -> tauri::Result<()> {
    window.with_webview(move |webview| {
        use gtk::{PageOrientation, PageSetup, PaperSize, PrintSettings, Unit};
        use webkit2gtk::{PrintOperation, PrintOperationExt};

        let uri = match gtk::glib::filename_to_uri(&path, None) {
            Ok(uri) => uri,
            Err(err) => {
                let _ = done.try_send(Err(err.to_string()));
                return;
            }
        };
        let paper = paper(&options);
        let size = match paper.name {
            Some(name) => PaperSize::new(Some(name)),
            None => PaperSize::new_custom("custom", "Custom", paper.width, paper.height, Unit::Mm),
        };
        let orientation =
            if options.is_landscape == Some(true) { PageOrientation::Landscape } else { PageOrientation::Portrait };

        let settings = PrintSettings::new();
        settings.set_printer(&gtk::glib::dgettext(Some("gtk30"), "Print to File"));
        settings.set(gtk::PRINT_SETTINGS_OUTPUT_URI, Some(&uri));
        settings.set(gtk::PRINT_SETTINGS_OUTPUT_FILE_FORMAT, Some("pdf"));
        settings.set_paper_size(&size);
        settings.set_orientation(orientation);

        let setup = PageSetup::new();
        setup.set_paper_size(&size);
        setup.set_orientation(orientation);
        set_margins(&setup, &margins);

        let operation = PrintOperation::new(&webview.inner());
        operation.set_print_settings(&settings);
        operation.set_page_setup(&setup);
        watch_operation(&operation, done);
        operation.print();
    })
}

// The dialog runs its own main loop until it closes. Started from an idle
// callback, that loop is not nested inside Tauri's event handling, which keeps
// working for the other windows meanwhile. The dialog starts with the page's
// margins on the system's default paper.
#[cfg(target_os = "linux")]
fn start_print(window: &tauri::WebviewWindow, margins: Margins, done: Done) -> tauri::Result<()> {
    window.with_webview(move |webview| {
        use gtk::prelude::{Cast, WidgetExt};
        use webkit2gtk::{PrintOperation, PrintOperationExt, PrintOperationResponse};

        let view = webview.inner();
        gtk::glib::idle_add_local_once(move || {
            let parent = view.toplevel().and_then(|widget| widget.downcast::<gtk::Window>().ok());
            let setup = gtk::PageSetup::new();
            set_margins(&setup, &margins);
            let operation = PrintOperation::new(&view);
            operation.set_page_setup(&setup);
            watch_operation(&operation, done.clone());
            if operation.run_dialog(parent.as_ref()) != PrintOperationResponse::Print {
                let _ = done.try_send(Ok(()));
            }
        });
    })
}

// The page size goes in inches; WebView2 turns it for landscape.
#[cfg(windows)]
fn start_pdf(
    window: &tauri::WebviewWindow,
    path: String,
    options: PageOptions,
    margins: Margins,
    done: Done,
) -> tauri::Result<()> {
    window.with_webview(move |webview| unsafe {
        use webview2_com::Microsoft::Web::WebView2::Win32::{
            ICoreWebView2Environment6, ICoreWebView2_7, COREWEBVIEW2_PRINT_ORIENTATION_LANDSCAPE,
            COREWEBVIEW2_PRINT_ORIENTATION_PORTRAIT,
        };
        use webview2_com::PrintToPdfCompletedHandler;
        use windows_core::{Interface, HSTRING};

        let reply = done.clone();
        let started = (|| -> windows_core::Result<()> {
            let core = webview.controller().CoreWebView2()?.cast::<ICoreWebView2_7>()?;
            let settings = webview.environment().cast::<ICoreWebView2Environment6>()?.CreatePrintSettings()?;
            let paper = paper(&options);
            settings.SetOrientation(if options.is_landscape == Some(true) {
                COREWEBVIEW2_PRINT_ORIENTATION_LANDSCAPE
            } else {
                COREWEBVIEW2_PRINT_ORIENTATION_PORTRAIT
            })?;
            settings.SetPageWidth(inches(paper.width))?;
            settings.SetPageHeight(inches(paper.height))?;
            settings.SetMarginTop(inches(margins.top))?;
            settings.SetMarginBottom(inches(margins.bottom))?;
            settings.SetMarginLeft(inches(margins.left))?;
            settings.SetMarginRight(inches(margins.right))?;
            settings.SetShouldPrintBackgrounds(true)?;
            // The exported page draws its own header and footer.
            settings.SetShouldPrintHeaderAndFooter(false)?;
            let handler = PrintToPdfCompletedHandler::create(Box::new(move |result, written| {
                let answer = match result {
                    Err(err) => Err(err.to_string()),
                    Ok(()) if !written => Err("WebView2 could not write the PDF".into()),
                    Ok(()) => Ok(()),
                };
                if let Err(err) = &answer {
                    eprintln!("[print] {err}");
                }
                let _ = reply.try_send(answer);
                Ok(())
            }));
            core.PrintToPdf(&HSTRING::from(path), &settings, &handler)
        })();
        if let Err(err) = started {
            eprintln!("[print] {err}");
            let _ = done.try_send(Err(format!("This WebView2 Runtime cannot export PDF files ({err}).")));
        }
    })
}

// The page prints itself with window.print() on Windows (src/export.ts):
// that call returns when the dialog closes, while ShowPrintUI gives no sign
// of when the page can be put back.
#[cfg(windows)]
fn start_print(_window: &tauri::WebviewWindow, _margins: Margins, done: Done) -> tauri::Result<()> {
    let _ = done.try_send(Err("printing goes through the page here".into()));
    Ok(())
}

#[cfg(not(any(target_os = "linux", windows)))]
fn start_pdf(
    _window: &tauri::WebviewWindow,
    _path: String,
    _options: PageOptions,
    _margins: Margins,
    done: Done,
) -> tauri::Result<()> {
    let _ = done.try_send(Err("exporting PDF files is not supported here".into()));
    Ok(())
}

#[cfg(not(any(target_os = "linux", windows)))]
fn start_print(_window: &tauri::WebviewWindow, _margins: Margins, done: Done) -> tauri::Result<()> {
    let _ = done.try_send(Err("printing is not supported here".into()));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(size: &str, width: Option<f64>, height: Option<f64>) -> PageOptions {
        PageOptions {
            page_size: Some(size.into()),
            page_size_width: width,
            page_size_height: height,
            is_landscape: None,
        }
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn named_sizes() {
        let a4 = paper(&options("A4", None, None));
        assert_eq!(a4, Paper { name: Some("iso_a4"), width: 210.0, height: 297.0 });
        assert_eq!(paper(&options("A3", None, None)).name, Some("iso_a3"));
        assert_eq!(paper(&options("A5", None, None)).width, 148.0);

        let letter = paper(&options("Letter", None, None));
        assert_eq!(letter.name, Some("na_letter"));
        assert!(close(inches(letter.width), 8.5) && close(inches(letter.height), 11.0));
        let legal = paper(&options("Legal", None, None));
        assert!(close(inches(legal.width), 8.5) && close(inches(legal.height), 14.0));
        let tabloid = paper(&options("Tabloid", None, None));
        assert!(close(inches(tabloid.width), 11.0) && close(inches(tabloid.height), 17.0));
    }

    #[test]
    fn custom_sizes_are_kept_in_mm() {
        let custom = paper(&options("custom", Some(100.0), Some(254.0)));
        assert_eq!(custom, Paper { name: None, width: 100.0, height: 254.0 });
        assert!(close(inches(custom.height), 10.0));
    }

    #[test]
    fn unknown_or_incomplete_sizes_become_a4() {
        let a4 = Some("iso_a4");
        assert_eq!(paper(&options("custom", Some(100.0), None)).name, a4);
        assert_eq!(paper(&options("custom", Some(0.0), Some(10.0))).name, a4);
        assert_eq!(paper(&options("B5", None, None)).name, a4);
        let none = PageOptions { page_size: None, page_size_width: None, page_size_height: None, is_landscape: None };
        assert_eq!(paper(&none).name, a4);
    }
}
