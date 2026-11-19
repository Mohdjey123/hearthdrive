//! Built-in Windows engines, used without downloading anything: reading
//! text in pictures and scanned PDFs (Windows.Media.Ocr, with the languages
//! installed in Windows), drawing PDF pages (Windows.Data.Pdf) and finding
//! faces in photos (Windows.Media.FaceAnalysis).
//!
//! Everything here blocks; call it from `spawn_blocking`.

#[cfg(windows)]
mod imp {
    use std::path::Path;
    use windows::Data::Pdf::{PdfDocument, PdfPageRenderOptions};
    use windows::Graphics::Imaging::{BitmapDecoder, BitmapPixelFormat, SoftwareBitmap};
    use windows::Media::FaceAnalysis::FaceDetector;
    use windows::Media::Ocr::OcrEngine;
    use windows::Storage::Streams::{DataWriter, IRandomAccessStream, InMemoryRandomAccessStream};
    use windows::Storage::{FileAccessMode, StorageFile};
    use windows::core::HSTRING;

    fn err(e: windows::core::Error) -> String {
        e.message().to_string()
    }

    fn open(path: &Path) -> Result<IRandomAccessStream, String> {
        let file = StorageFile::GetFileFromPathAsync(&HSTRING::from(path.as_os_str())).map_err(err)?.join().map_err(err)?;
        file.OpenAsync(FileAccessMode::Read).map_err(err)?.join().map_err(err)
    }

    fn from_bytes(bytes: &[u8]) -> Result<IRandomAccessStream, String> {
        let stream = InMemoryRandomAccessStream::new().map_err(err)?;
        let writer = DataWriter::CreateDataWriter(&stream).map_err(err)?;
        writer.WriteBytes(bytes).map_err(err)?;
        writer.StoreAsync().map_err(err)?.join().map_err(err)?;
        writer.DetachStream().map_err(err)?;
        stream.Seek(0).map_err(err)?;
        let stream: IRandomAccessStream = windows::core::Interface::cast(&stream).map_err(err)?;
        Ok(stream)
    }

    fn bitmap(stream: &IRandomAccessStream) -> Result<SoftwareBitmap, String> {
        let decoder = BitmapDecoder::CreateAsync(stream).map_err(err)?.join().map_err(err)?;
        decoder.GetSoftwareBitmapAsync().map_err(err)?.join().map_err(err)
    }

    fn engine() -> Result<OcrEngine, String> {
        OcrEngine::TryCreateFromUserProfileLanguages().map_err(|_| "No text recognition language is installed in Windows".to_string())
    }

    fn recognize(engine: &OcrEngine, bitmap: &SoftwareBitmap) -> Result<String, String> {
        let max = OcrEngine::MaxImageDimension().unwrap_or(10_000) as i32;
        if bitmap.PixelWidth().unwrap_or(0) > max || bitmap.PixelHeight().unwrap_or(0) > max {
            return Err("The picture is too big to read text in".into());
        }
        let result = engine.RecognizeAsync(bitmap).map_err(err)?.join().map_err(err)?;
        let mut out = String::new();
        for line in result.Lines().map_err(err)? {
            out.push_str(&line.Text().map_err(err)?.to_string_lossy());
            out.push('\n');
        }
        Ok(out)
    }

    /// Text in a picture file (PNG, JPEG, TIFF, BMP, GIF).
    pub fn ocr_image(path: &Path) -> Result<String, String> {
        let engine = engine()?;
        recognize(&engine, &bitmap(&open(path)?)?)
    }

    /// Text in a picture held in memory.
    pub fn ocr_bytes(bytes: &[u8]) -> Result<String, String> {
        let engine = engine()?;
        recognize(&engine, &bitmap(&from_bytes(bytes)?)?)
    }

    /// Text on the first `max_pages` pages of a PDF, read from the drawn
    /// pages (for scanned documents without a text layer).
    pub fn ocr_pdf(path: &Path, max_pages: u32) -> Result<String, String> {
        let engine = engine()?;
        let file = StorageFile::GetFileFromPathAsync(&HSTRING::from(path.as_os_str())).map_err(err)?.join().map_err(err)?;
        let doc = PdfDocument::LoadFromFileAsync(&file).map_err(err)?.join().map_err(err)?;
        let mut out = String::new();
        for i in 0..doc.PageCount().map_err(err)?.min(max_pages) {
            let page = doc.GetPage(i).map_err(err)?;
            let stream = InMemoryRandomAccessStream::new().map_err(err)?;
            let options = PdfPageRenderOptions::new().map_err(err)?;
            // About 200 dpi for an A4 page: enough for small print.
            options.SetDestinationWidth(1700).map_err(err)?;
            page.RenderWithOptionsToStreamAsync(&stream, &options).map_err(err)?.join().map_err(err)?;
            stream.Seek(0).map_err(err)?;
            let stream: IRandomAccessStream = windows::core::Interface::cast(&stream).map_err(err)?;
            out.push_str(&recognize(&engine, &bitmap(&stream)?)?);
            out.push('\n');
        }
        Ok(out)
    }

    /// How many faces a photo shows (from its JPEG preview).
    pub fn count_faces(jpeg: &[u8]) -> Result<u32, String> {
        let detector = FaceDetector::CreateAsync().map_err(err)?.join().map_err(err)?;
        let bmp = bitmap(&from_bytes(jpeg)?)?;
        let gray = SoftwareBitmap::Convert(&bmp, BitmapPixelFormat::Gray8).map_err(err)?;
        let faces = detector.DetectFacesAsync(&gray).map_err(err)?.join().map_err(err)?;
        faces.Size().map_err(err)
    }

    pub fn available() -> bool {
        OcrEngine::TryCreateFromUserProfileLanguages().is_ok()
    }
}

#[cfg(not(windows))]
mod imp {
    use std::path::Path;
    const NO: &str = "Text recognition needs Windows";
    pub fn ocr_image(_: &Path) -> Result<String, String> {
        Err(NO.into())
    }
    pub fn ocr_bytes(_: &[u8]) -> Result<String, String> {
        Err(NO.into())
    }
    pub fn ocr_pdf(_: &Path, _: u32) -> Result<String, String> {
        Err(NO.into())
    }
    pub fn count_faces(_: &[u8]) -> Result<u32, String> {
        Err(NO.into())
    }
    pub fn available() -> bool {
        false
    }
}

pub use imp::*;

#[cfg(all(test, windows))]
mod tests {
    #[test]
    fn windows_engines_run() {
        let mut png = Vec::new();
        image::RgbImage::from_pixel(200, 120, image::Rgb([255, 255, 255]))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        assert_eq!(super::count_faces(&png).unwrap(), 0);
        if super::available() {
            assert!(super::ocr_bytes(&png).unwrap().trim().is_empty());
        }
    }
}
