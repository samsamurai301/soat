//! PDF/A-3b documents: pages of text and rules in Liberation Sans, with
//! files attached (a Factur-X invoice's XML, see [`crate::facturx`]).
//!
//! Only what archival invoices need: embedded, subset TrueType fonts
//! (PDF/A allows no others), an sRGB output intent, XMP metadata and
//! associated files. Text is encoded as WinAnsi; a character outside it is
//! written as `?`.
//!
//! ```
//! use soat_core::pdf::{Pdf, Style};
//!
//! let mut pdf = Pdf::new("Hello");
//! let page = pdf.page();
//! page.text(Style::Bold, 18.0, 50.0, 780.0, "Hello, Grüße");
//! let bytes = pdf.finish("2026-11-27", &[], "");
//! assert!(bytes.starts_with(b"%PDF-1.7"));
//! ```

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::io::Write as _;
use std::sync::OnceLock;

use sha2::{Digest, Sha256};

/// A4, in points.
pub const A4: (f32, f32) = (595.28, 841.89);

static REGULAR_TTF: &[u8] = include_bytes!("../assets/fonts/LiberationSans-Regular.ttf");
static BOLD_TTF: &[u8] = include_bytes!("../assets/fonts/LiberationSans-Bold.ttf");

/// Which face text is set in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Style {
    Regular,
    Bold,
}

impl Style {
    fn font(self) -> &'static Font {
        static REGULAR: OnceLock<Font> = OnceLock::new();
        static BOLD: OnceLock<Font> = OnceLock::new();
        match self {
            Style::Regular => REGULAR.get_or_init(|| {
                Font::parse(REGULAR_TTF, "LiberationSans").expect("the bundled font parses")
            }),
            Style::Bold => BOLD.get_or_init(|| {
                Font::parse(BOLD_TTF, "LiberationSans-Bold").expect("the bundled font parses")
            }),
        }
    }

    fn resource(self) -> &'static str {
        match self {
            Style::Regular => "F1",
            Style::Bold => "F2",
        }
    }
}

/// How wide `text` is in `style` at `size` points.
pub fn width(style: Style, size: f32, text: &str) -> f32 {
    let font = style.font();
    let units: u32 = winansi(text)
        .iter()
        .map(|&c| u32::from(font.width(c)))
        .sum();
    #[allow(clippy::cast_precision_loss)]
    let w = units as f32 * size / 1000.0;
    w
}

/// `text` cut to fit `max` points, with `…` when it was cut.
pub fn fit(style: Style, size: f32, text: &str, max: f32) -> String {
    if width(style, size, text) <= max {
        return text.to_owned();
    }
    let mut out: String = text.to_owned();
    while !out.is_empty() && width(style, size, &format!("{out}…")) > max {
        out.pop();
    }
    format!("{}…", out.trim_end())
}

/// `text` broken into lines of at most `max` points, at spaces.
pub fn wrap(style: Style, size: f32, text: &str, max: f32) -> Vec<String> {
    let mut lines = Vec::new();
    for paragraph in text.lines() {
        let mut line = String::new();
        for word in paragraph.split_whitespace() {
            let candidate = if line.is_empty() {
                word.to_owned()
            } else {
                format!("{line} {word}")
            };
            if width(style, size, &candidate) <= max || line.is_empty() {
                line = candidate;
            } else {
                lines.push(std::mem::take(&mut line));
                line = word.to_owned();
            }
        }
        lines.push(fit(style, size, &line, max));
    }
    lines
}

/// A file attached to the document (PDF/A-3 associated file).
pub struct Attachment<'a> {
    /// The file name, e.g. `factur-x.xml`.
    pub name: &'a str,
    /// Its MIME type, e.g. `text/xml`.
    pub mime: &'a str,
    pub description: &'a str,
    /// How it relates to the document: `Alternative`, `Data`, `Source`...
    pub relationship: &'a str,
    pub data: &'a [u8],
}

/// One page's drawing.
#[derive(Default)]
pub struct Page {
    ops: String,
    used: BTreeMap<Style, BTreeSet<u8>>,
}

impl Page {
    /// `text` with its baseline starting at `x`, `y` (from the bottom left).
    pub fn text(&mut self, style: Style, size: f32, x: f32, y: f32, text: &str) {
        let codes = winansi(text);
        self.used.entry(style).or_default().extend(&codes);
        let _ = write!(
            self.ops,
            "BT /{} {} Tf {} {} Td (",
            style.resource(),
            num(size),
            num(x),
            num(y)
        );
        for c in codes {
            match c {
                b'(' | b')' | b'\\' => {
                    self.ops.push('\\');
                    self.ops.push(char::from(c));
                }
                32..=126 => self.ops.push(char::from(c)),
                _ => {
                    let _ = write!(self.ops, "\\{c:03o}");
                }
            }
        }
        self.ops.push_str(") Tj ET\n");
    }

    /// `text` ending at `x`.
    pub fn text_right(&mut self, style: Style, size: f32, x: f32, y: f32, text: &str) {
        self.text(style, size, x - width(style, size, text), y, text);
    }

    /// A straight rule, `w` points wide, in gray `gray` (0 black, 1 white).
    pub fn line(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, w: f32, gray: f32) {
        let _ = writeln!(
            self.ops,
            "q {} G {} w {} {} m {} {} l S Q",
            num(gray),
            num(w),
            num(x1),
            num(y1),
            num(x2),
            num(y2)
        );
    }
}

/// A document being drawn.
pub struct Pdf {
    title: String,
    pages: Vec<Page>,
    used: BTreeMap<Style, BTreeSet<u8>>,
}

impl Pdf {
    /// An empty document titled `title`.
    pub fn new(title: &str) -> Self {
        Self {
            title: title.to_owned(),
            pages: Vec::new(),
            used: BTreeMap::new(),
        }
    }

    /// A new A4 page, to draw on.
    pub fn page(&mut self) -> &mut Page {
        self.pages.push(Page::default());
        self.pages.last_mut().expect("just pushed")
    }

    /// The last page, or a new one when there is none.
    pub fn current(&mut self) -> &mut Page {
        if self.pages.is_empty() {
            self.pages.push(Page::default());
        }
        self.pages.last_mut().expect("there is a page")
    }

    /// Page `n`, from 0.
    ///
    /// # Panics
    ///
    /// When there is no such page.
    pub fn page_at(&mut self, n: usize) -> &mut Page {
        &mut self.pages[n]
    }

    /// How many pages there are.
    pub fn pages(&self) -> usize {
        self.pages.len()
    }

    /// The finished PDF/A-3b file. `date` (`YYYY-MM-DD`) is its creation
    /// date; `xmp` holds extra `rdf:Description`s for its metadata.
    pub fn finish(mut self, date: &str, files: &[Attachment<'_>], xmp: &str) -> Vec<u8> {
        if self.pages.is_empty() {
            self.page();
        }
        for page in &self.pages {
            for (style, codes) in &page.used {
                self.used.entry(*style).or_default().extend(codes);
            }
        }
        let mut w = Writer::default();
        let catalog = w.reserve();
        let pages = w.reserve();

        // Fonts.
        let mut fonts = Vec::new();
        for style in [Style::Regular, Style::Bold] {
            let codes = self.used.get(&style).cloned().unwrap_or_default();
            fonts.push((style, font_objects(&mut w, style.font(), &codes)));
        }
        let mut font_res = String::from("<< ");
        for (style, id) in &fonts {
            let _ = write!(font_res, "/{} {id} 0 R ", style.resource());
        }
        font_res.push_str(">>");

        // Pages.
        let mut kids = Vec::new();
        for page in &self.pages {
            let content = w.stream("", page.ops.as_bytes(), true);
            let id = w.object(&format!(
                "<< /Type /Page /Parent {pages} 0 R /MediaBox [0 0 {} {}] \
                 /Resources << /Font {font_res} >> /Contents {content} 0 R >>",
                num(A4.0),
                num(A4.1)
            ));
            kids.push(format!("{id} 0 R"));
        }
        w.set(
            pages,
            &format!(
                "<< /Type /Pages /Kids [{}] /Count {} >>",
                kids.join(" "),
                kids.len()
            ),
        );

        // Attached files.
        let pdf_date = format!("D:{}000000Z", date.replace('-', ""));
        let mut specs = Vec::new();
        for f in files {
            let ef = w.stream(
                &format!(
                    "/Type /EmbeddedFile /Subtype /{} /Params << /ModDate {} /Size {} >>",
                    name_escape(f.mime),
                    string(&pdf_date),
                    f.data.len()
                ),
                f.data,
                true,
            );
            let spec = w.object(&format!(
                "<< /Type /Filespec /F {name} /UF {name} /Desc {} /AFRelationship /{} \
                 /EF << /F {ef} 0 R /UF {ef} 0 R >> >>",
                string(f.description),
                f.relationship,
                name = string(f.name),
            ));
            specs.push((f.name, spec));
        }

        // Metadata and the colour profile.
        let iso = format!("{date}T00:00:00Z");
        let metadata = w.stream(
            "/Type /Metadata /Subtype /XML",
            metadata(&self.title, &iso, xmp).as_bytes(),
            false,
        );
        let icc = w.stream("/N 3", &srgb_profile(), true);
        let intent = w.object(&format!(
            "<< /Type /OutputIntent /S /GTS_PDFA1 /OutputConditionIdentifier (sRGB) \
             /Info (sRGB IEC61966-2.1) /DestOutputProfile {icc} 0 R >>"
        ));
        let mut catalog_dict = format!(
            "<< /Type /Catalog /Pages {pages} 0 R /Metadata {metadata} 0 R \
             /OutputIntents [{intent} 0 R]"
        );
        if !specs.is_empty() {
            let names: Vec<String> = specs
                .iter()
                .map(|(n, id)| format!("{} {id} 0 R", string(n)))
                .collect();
            let refs: Vec<String> = specs.iter().map(|(_, id)| format!("{id} 0 R")).collect();
            let _ = write!(
                catalog_dict,
                " /Names << /EmbeddedFiles << /Names [{}] >> >> /AF [{}]",
                names.join(" "),
                refs.join(" ")
            );
        }
        catalog_dict.push_str(" >>");
        w.set(catalog, &catalog_dict);
        w.finish(catalog)
    }
}

fn num(v: f32) -> String {
    let s = format!("{v:.2}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" {
        "0".to_owned()
    } else {
        s.to_owned()
    }
}

/// A PDF literal string, with every byte outside printable ASCII escaped.
fn string(text: &str) -> String {
    let mut out = String::from("(");
    for c in winansi(text) {
        match c {
            b'(' | b')' | b'\\' => {
                out.push('\\');
                out.push(char::from(c));
            }
            32..=126 => out.push(char::from(c)),
            _ => {
                let _ = write!(out, "\\{c:03o}");
            }
        }
    }
    out.push(')');
    out
}

/// A PDF name's characters: `/` and the like as `#xx`.
fn name_escape(name: &str) -> String {
    name.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b == b'-' || b == b'.' || b == b'_' || b == b'+' {
                char::from(b).to_string()
            } else {
                format!("#{b:02X}")
            }
        })
        .collect()
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn metadata(title: &str, date: &str, extra: &str) -> String {
    let title = xml_escape(title);
    format!(
        "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n\
<x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\n\
<rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n\
<rdf:Description rdf:about=\"\" xmlns:pdfaid=\"http://www.aiim.org/pdfa/ns/id/\">\
<pdfaid:part>3</pdfaid:part><pdfaid:conformance>B</pdfaid:conformance></rdf:Description>\n\
<rdf:Description rdf:about=\"\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\">\
<dc:format>application/pdf</dc:format>\
<dc:title><rdf:Alt><rdf:li xml:lang=\"x-default\">{title}</rdf:li></rdf:Alt></dc:title></rdf:Description>\n\
<rdf:Description rdf:about=\"\" xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\">\
<xmp:CreateDate>{date}</xmp:CreateDate><xmp:ModifyDate>{date}</xmp:ModifyDate>\
<xmp:MetadataDate>{date}</xmp:MetadataDate><xmp:CreatorTool>SOAT</xmp:CreatorTool></rdf:Description>\n\
<rdf:Description rdf:about=\"\" xmlns:pdf=\"http://ns.adobe.com/pdf/1.3/\">\
<pdf:Producer>SOAT</pdf:Producer></rdf:Description>\n\
{extra}\
</rdf:RDF>\n</x:xmpmeta>\n<?xpacket end=\"w\"?>"
    )
}

/// The objects of a file, numbered from 1.
#[derive(Default)]
struct Writer {
    objects: Vec<Option<Vec<u8>>>,
}

impl Writer {
    fn reserve(&mut self) -> usize {
        self.objects.push(None);
        self.objects.len()
    }

    fn set(&mut self, id: usize, body: &str) {
        self.objects[id - 1] = Some(body.as_bytes().to_vec());
    }

    fn object(&mut self, body: &str) -> usize {
        let id = self.reserve();
        self.set(id, body);
        id
    }

    /// A stream with `dict` entries, compressed when `deflate`.
    fn stream(&mut self, dict: &str, data: &[u8], deflate: bool) -> usize {
        self.stream_with(dict, data, deflate, None)
    }

    fn stream_with(
        &mut self,
        dict: &str,
        data: &[u8],
        deflate: bool,
        length1: Option<usize>,
    ) -> usize {
        let body = if deflate {
            let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
            z.write_all(data).expect("writing to memory");
            z.finish().expect("writing to memory")
        } else {
            data.to_vec()
        };
        let mut out = format!("<< {dict} /Length {}", body.len());
        if deflate {
            out.push_str(" /Filter /FlateDecode");
        }
        if let Some(n) = length1 {
            let _ = write!(out, " /Length1 {n}");
        }
        out.push_str(" >>\nstream\n");
        let mut bytes = out.into_bytes();
        bytes.extend_from_slice(&body);
        bytes.extend_from_slice(b"\nendstream");
        let id = self.reserve();
        self.objects[id - 1] = Some(bytes);
        id
    }

    fn finish(self, root: usize) -> Vec<u8> {
        let mut out = b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n".to_vec();
        let mut offsets = Vec::new();
        for (i, body) in self.objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
            out.extend_from_slice(body.as_deref().unwrap_or(b"null"));
            out.extend_from_slice(b"\nendobj\n");
        }
        let id = hex(&Sha256::digest(&out)[..16]);
        let xref = out.len();
        let mut table = format!("xref\n0 {}\n0000000000 65535 f \n", self.objects.len() + 1);
        for o in offsets {
            let _ = writeln!(table, "{o:010} 00000 n ");
        }
        let _ = write!(
            table,
            "trailer\n<< /Size {} /Root {root} 0 R /ID [<{id}> <{id}>] >>\nstartxref\n{xref}\n%%EOF\n",
            self.objects.len() + 1
        );
        out.extend_from_slice(table.as_bytes());
        out
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// The font dictionary of `font` with the glyphs of `codes` embedded.
fn font_objects(w: &mut Writer, font: &Font, codes: &BTreeSet<u8>) -> usize {
    let glyphs: BTreeSet<u16> = codes.iter().map(|&c| font.glyph(c)).collect();
    let program = font.subset(&glyphs);
    let tag: String = Sha256::digest(&program)[..6]
        .iter()
        .map(|b| char::from(b'A' + b % 26))
        .collect();
    let name = format!("{tag}+{}", font.name);
    let file = w.stream_with("", &program, true, Some(program.len()));
    let scale = |v: i16| i32::from(v) * 1000 / i32::from(font.units_per_em);
    let descriptor = w.object(&format!(
        "<< /Type /FontDescriptor /FontName /{name} /Flags 32 /FontBBox [{} {} {} {}] \
         /ItalicAngle 0 /Ascent {} /Descent {} /CapHeight {} /StemV {} /FontFile2 {file} 0 R >>",
        scale(font.bbox[0]),
        scale(font.bbox[1]),
        scale(font.bbox[2]),
        scale(font.bbox[3]),
        scale(font.ascent),
        scale(font.descent),
        scale(font.cap_height),
        if font.bold { 140 } else { 80 },
    ));
    let widths: Vec<String> = (32..=255u8).map(|c| font.width(c).to_string()).collect();
    w.object(&format!(
        "<< /Type /Font /Subtype /TrueType /BaseFont /{name} /FirstChar 32 /LastChar 255 \
         /Widths [{}] /Encoding /WinAnsiEncoding /FontDescriptor {descriptor} 0 R >>",
        widths.join(" ")
    ))
}

/// Unicode to WinAnsi (CP-1252); `?` for what it cannot hold.
pub fn winansi(text: &str) -> Vec<u8> {
    text.chars()
        .map(|c| match c as u32 {
            0x20..=0x7E | 0xA0..=0xFF => c as u8,
            0x20AC => 0x80,
            0x201A => 0x82,
            0x0192 => 0x83,
            0x201E => 0x84,
            0x2026 => 0x85,
            0x2020 => 0x86,
            0x2021 => 0x87,
            0x02C6 => 0x88,
            0x2030 => 0x89,
            0x0160 => 0x8A,
            0x2039 => 0x8B,
            0x0152 => 0x8C,
            0x017D => 0x8E,
            0x2018 => 0x91,
            0x2019 => 0x92,
            0x201C => 0x93,
            0x201D => 0x94,
            0x2022 => 0x95,
            0x2013 => 0x96,
            0x2014 => 0x97,
            0x02DC => 0x98,
            0x2122 => 0x99,
            0x0161 => 0x9A,
            0x203A => 0x9B,
            0x0153 => 0x9C,
            0x017E => 0x9E,
            0x0178 => 0x9F,
            0x09 | 0x0A | 0x0D => b' ',
            _ => b'?',
        })
        .collect()
}

/// The Unicode character of a WinAnsi code.
fn unicode_of(code: u8) -> u32 {
    const HIGH: [u32; 32] = [
        0x20AC, 0, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160, 0x2039,
        0x0152, 0, 0x017D, 0, 0, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014, 0x02DC,
        0x2122, 0x0161, 0x203A, 0x0153, 0, 0x017E, 0x0178,
    ];
    match code {
        0x80..=0x9F => HIGH[usize::from(code - 0x80)],
        _ => u32::from(code),
    }
}

/// A parsed TrueType font: its metrics, and the glyphs for WinAnsi codes.
struct Font {
    data: &'static [u8],
    name: &'static str,
    tables: BTreeMap<[u8; 4], (usize, usize)>,
    units_per_em: u16,
    bbox: [i16; 4],
    ascent: i16,
    descent: i16,
    cap_height: i16,
    bold: bool,
    /// Glyph and advance (in 1000ths of an em) of each WinAnsi code.
    codes: Vec<(u16, u16)>,
}

fn u16_at(d: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([d[at], d[at + 1]])
}

fn i16_at(d: &[u8], at: usize) -> i16 {
    i16::from_be_bytes([d[at], d[at + 1]])
}

fn u32_at(d: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([d[at], d[at + 1], d[at + 2], d[at + 3]])
}

impl Font {
    fn parse(data: &'static [u8], name: &'static str) -> Option<Self> {
        let n = usize::from(u16_at(data, 4));
        let mut tables = BTreeMap::new();
        for i in 0..n {
            let rec = 12 + i * 16;
            let tag: [u8; 4] = data.get(rec..rec + 4)?.try_into().ok()?;
            let offset = u32_at(data, rec + 8) as usize;
            let len = u32_at(data, rec + 12) as usize;
            data.get(offset..offset + len)?;
            tables.insert(tag, (offset, len));
        }
        let t = |tag: &[u8; 4]| tables.get(tag).map(|&(o, _)| o);
        let head = t(b"head")?;
        let hhea = t(b"hhea")?;
        let os2 = t(b"OS/2")?;
        let hmtx = t(b"hmtx")?;
        let cmap = t(b"cmap")?;
        let units_per_em = u16_at(data, head + 18);
        let bbox = [
            i16_at(data, head + 36),
            i16_at(data, head + 38),
            i16_at(data, head + 40),
            i16_at(data, head + 42),
        ];
        let metrics = usize::from(u16_at(data, hhea + 34));
        let advance = |glyph: u16| {
            let g = usize::from(glyph).min(metrics - 1);
            u32::from(u16_at(data, hmtx + g * 4))
        };
        // The (3, 1) format 4 subtable: Unicode BMP.
        let mut sub = None;
        for i in 0..usize::from(u16_at(data, cmap + 2)) {
            let rec = cmap + 4 + i * 8;
            if u16_at(data, rec) == 3 && u16_at(data, rec + 2) == 1 {
                let at = cmap + u32_at(data, rec + 4) as usize;
                if u16_at(data, at) == 4 {
                    sub = Some(at);
                }
            }
        }
        let sub = sub?;
        let segs = usize::from(u16_at(data, sub + 6)) / 2;
        let glyph_of = |u: u32| -> u16 {
            if u == 0 || u > 0xFFFF {
                return 0;
            }
            let ends = sub + 14;
            let starts = ends + segs * 2 + 2;
            let deltas = starts + segs * 2;
            let ranges = deltas + segs * 2;
            for s in 0..segs {
                let end = u32::from(u16_at(data, ends + s * 2));
                if u > end {
                    continue;
                }
                let start = u32::from(u16_at(data, starts + s * 2));
                if u < start {
                    return 0;
                }
                let delta = u16_at(data, deltas + s * 2);
                let range = usize::from(u16_at(data, ranges + s * 2));
                #[allow(clippy::cast_possible_truncation)]
                let g = if range == 0 {
                    (u as u16).wrapping_add(delta)
                } else {
                    let at = ranges + s * 2 + range + (u - start) as usize * 2;
                    let g = u16_at(data, at);
                    if g == 0 {
                        0
                    } else {
                        g.wrapping_add(delta)
                    }
                };
                return g;
            }
            0
        };
        let codes = (0..=255u8)
            .map(|c| {
                let g = glyph_of(unicode_of(c));
                #[allow(clippy::cast_possible_truncation)]
                let w = (advance(g) * 1000 / u32::from(units_per_em)) as u16;
                (g, w)
            })
            .collect();
        let os2_version = u16_at(data, os2);
        Some(Self {
            data,
            name,
            units_per_em,
            bbox,
            ascent: i16_at(data, hhea + 4),
            descent: i16_at(data, hhea + 6),
            cap_height: if os2_version >= 2 {
                i16_at(data, os2 + 88)
            } else {
                bbox[3]
            },
            bold: u16_at(data, os2 + 4) >= 600,
            tables,
            codes,
        })
    }

    fn glyph(&self, code: u8) -> u16 {
        self.codes[usize::from(code)].0
    }

    fn width(&self, code: u8) -> u16 {
        self.codes[usize::from(code)].1
    }

    fn table(&self, tag: &[u8; 4]) -> Option<&'static [u8]> {
        self.tables.get(tag).map(|&(o, l)| &self.data[o..o + l])
    }

    /// The font with only `glyphs` (and `.notdef` and the parts of
    /// composite glyphs) kept: the other glyphs are empty.
    fn subset(&self, glyphs: &BTreeSet<u16>) -> Vec<u8> {
        let (Some(head), Some(loca), Some(glyf), Some(maxp)) = (
            self.table(b"head"),
            self.table(b"loca"),
            self.table(b"glyf"),
            self.table(b"maxp"),
        ) else {
            return self.data.to_vec();
        };
        let long = i16_at(head, 50) == 1;
        let count = usize::from(u16_at(maxp, 4));
        let at = |g: usize| -> usize {
            if long {
                u32_at(loca, g * 4) as usize
            } else {
                usize::from(u16_at(loca, g * 2)) * 2
            }
        };
        // Glyphs to keep, with the components of composites.
        let mut keep: BTreeSet<u16> = glyphs.clone();
        keep.insert(0);
        let mut todo: Vec<u16> = keep.iter().copied().collect();
        while let Some(g) = todo.pop() {
            let g = usize::from(g);
            if g >= count {
                continue;
            }
            let (start, end) = (at(g), at(g + 1));
            if end <= start || i16_at(glyf, start) >= 0 {
                continue;
            }
            let mut p = start + 10;
            loop {
                let flags = u16_at(glyf, p);
                let component = u16_at(glyf, p + 2);
                if keep.insert(component) {
                    todo.push(component);
                }
                p += 4 + if flags & 1 != 0 { 4 } else { 2 };
                p += if flags & 8 != 0 {
                    2
                } else if flags & 0x40 != 0 {
                    4
                } else if flags & 0x80 != 0 {
                    8
                } else {
                    0
                };
                if flags & 0x20 == 0 {
                    break;
                }
            }
        }
        let mut new_glyf = Vec::new();
        let mut new_loca = Vec::with_capacity((count + 1) * 4);
        for g in 0..count {
            #[allow(clippy::cast_possible_truncation)]
            new_loca.extend_from_slice(&(new_glyf.len() as u32).to_be_bytes());
            #[allow(clippy::cast_possible_truncation)]
            if keep.contains(&(g as u16)) {
                new_glyf.extend_from_slice(&glyf[at(g)..at(g + 1)]);
                while new_glyf.len() % 4 != 0 {
                    new_glyf.push(0);
                }
            }
        }
        #[allow(clippy::cast_possible_truncation)]
        new_loca.extend_from_slice(&(new_glyf.len() as u32).to_be_bytes());
        let mut new_head = head.to_vec();
        new_head[8..12].copy_from_slice(&[0; 4]);
        new_head[50..52].copy_from_slice(&1i16.to_be_bytes());

        let mut out_tables: Vec<([u8; 4], Vec<u8>)> = Vec::new();
        for tag in [
            b"OS/2", b"cmap", b"cvt ", b"fpgm", b"glyf", b"head", b"hhea", b"hmtx", b"loca",
            b"maxp", b"name", b"post", b"prep",
        ] {
            let data = match tag {
                b"glyf" => new_glyf.clone(),
                b"loca" => new_loca.clone(),
                b"head" => new_head.clone(),
                _ => match self.table(tag) {
                    Some(t) => t.to_vec(),
                    None => continue,
                },
            };
            out_tables.push((*tag, data));
        }
        let n = out_tables.len();
        let mut power = 1;
        let mut log = 0u16;
        while power * 2 <= n {
            power *= 2;
            log += 1;
        }
        #[allow(clippy::cast_possible_truncation)]
        let header = [
            0x0001_0000u32.to_be_bytes().to_vec(),
            (n as u16).to_be_bytes().to_vec(),
            ((power * 16) as u16).to_be_bytes().to_vec(),
            log.to_be_bytes().to_vec(),
            ((n * 16 - power * 16) as u16).to_be_bytes().to_vec(),
        ]
        .concat();
        let mut out = header;
        let mut offset = 12 + n * 16;
        let mut bodies = Vec::new();
        for (tag, data) in &out_tables {
            let mut padded = data.clone();
            while padded.len() % 4 != 0 {
                padded.push(0);
            }
            out.extend_from_slice(tag);
            out.extend_from_slice(&checksum(&padded).to_be_bytes());
            #[allow(clippy::cast_possible_truncation)]
            {
                out.extend_from_slice(&(offset as u32).to_be_bytes());
                out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            }
            offset += padded.len();
            bodies.push(padded);
        }
        for b in bodies {
            out.extend_from_slice(&b);
        }
        // head.checkSumAdjustment makes the whole file sum to 0xB1B0AFBA.
        let adjust = 0xB1B0_AFBAu32.wrapping_sub(checksum(&out));
        if let Some(pos) = out_tables.iter().position(|(t, _)| t == b"head") {
            let rec = 12 + pos * 16;
            let head_at = u32_at(&out, rec + 8) as usize;
            out[head_at + 8..head_at + 12].copy_from_slice(&adjust.to_be_bytes());
        }
        out
    }
}

fn checksum(data: &[u8]) -> u32 {
    data.chunks(4).fold(0u32, |sum, c| {
        let mut word = [0u8; 4];
        word[..c.len()].copy_from_slice(c);
        sum.wrapping_add(u32::from_be_bytes(word))
    })
}

/// A small ICC v2 display profile for sRGB (D50 primaries, gamma 2.2), the
/// output intent PDF/A asks for.
fn srgb_profile() -> Vec<u8> {
    fn s15(v: f64) -> [u8; 4] {
        #[allow(clippy::cast_possible_truncation)]
        let fixed = (v * 65536.0).round() as i32;
        fixed.to_be_bytes()
    }
    fn xyz(x: f64, y: f64, z: f64) -> Vec<u8> {
        [b"XYZ \0\0\0\0".as_slice(), &s15(x), &s15(y), &s15(z)].concat()
    }
    let desc = {
        let text = b"sRGB IEC61966-2.1\0";
        let mut d = b"desc\0\0\0\0".to_vec();
        #[allow(clippy::cast_possible_truncation)]
        d.extend_from_slice(&(text.len() as u32).to_be_bytes());
        d.extend_from_slice(text);
        d.extend_from_slice(&[0; 4 + 4 + 2 + 1 + 67]);
        d
    };
    let cprt = [b"text\0\0\0\0".as_slice(), b"No copyright, use freely\0"].concat();
    let curve = [
        b"curv\0\0\0\0".as_slice(),
        &1u32.to_be_bytes(),
        &[0x02, 0x33],
    ]
    .concat();
    let tags: Vec<(&[u8; 4], Vec<u8>)> = vec![
        (b"desc", desc),
        (b"cprt", cprt),
        (b"wtpt", xyz(0.9642, 1.0, 0.8249)),
        (b"rXYZ", xyz(0.4361, 0.2225, 0.0139)),
        (b"gXYZ", xyz(0.3851, 0.7169, 0.0971)),
        (b"bXYZ", xyz(0.1431, 0.0606, 0.7141)),
        (b"rTRC", curve.clone()),
        (b"gTRC", curve.clone()),
        (b"bTRC", curve),
    ];
    let mut table = Vec::new();
    let mut data = Vec::new();
    let mut offset = 128 + 4 + tags.len() * 12;
    for (sig, body) in &tags {
        table.extend_from_slice(*sig);
        #[allow(clippy::cast_possible_truncation)]
        {
            table.extend_from_slice(&(offset as u32).to_be_bytes());
            table.extend_from_slice(&(body.len() as u32).to_be_bytes());
        }
        let mut padded = body.clone();
        while padded.len() % 4 != 0 {
            padded.push(0);
        }
        offset += padded.len();
        data.extend_from_slice(&padded);
    }
    let size = offset;
    let mut out = Vec::with_capacity(size);
    #[allow(clippy::cast_possible_truncation)]
    out.extend_from_slice(&(size as u32).to_be_bytes());
    out.extend_from_slice(&[0; 4]); // CMM
    out.extend_from_slice(&[0x02, 0x10, 0, 0]); // version 2.1
    out.extend_from_slice(b"mntrRGB XYZ ");
    out.extend_from_slice(&[0x07, 0xEA, 0, 11, 0, 27, 0, 0, 0, 0, 0, 0]); // 2026-11-27
    out.extend_from_slice(b"acsp");
    out.extend_from_slice(&[0; 4 + 4 + 4 + 4 + 8 + 4]); // platform .. intent
    out.extend_from_slice(&s15(0.9642));
    out.extend_from_slice(&s15(1.0));
    out.extend_from_slice(&s15(0.8249));
    out.extend_from_slice(&[0; 4 + 16 + 28]); // creator, id, reserved
    #[allow(clippy::cast_possible_truncation)]
    out.extend_from_slice(&(tags.len() as u32).to_be_bytes());
    out.extend_from_slice(&table);
    out.extend_from_slice(&data);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_winansi() {
        assert_eq!(winansi("Grüße €5 – ok"), b"Gr\xfc\xdfe \x805 \x96 ok");
        assert_eq!(winansi("日本"), b"??");
        for c in 0..=255u8 {
            let u = unicode_of(c);
            if u != 0 && c >= 32 && c != 127 {
                let s = char::from_u32(u).unwrap().to_string();
                assert_eq!(winansi(&s), [c], "code {c}");
            }
        }
    }

    #[test]
    fn the_fonts_measure_text() {
        let a = width(Style::Regular, 10.0, "Invoice");
        let b = width(Style::Bold, 10.0, "Invoice");
        assert!(a > 25.0 && a < 45.0, "{a}");
        assert!(b > a);
        assert_eq!(width(Style::Regular, 10.0, ""), 0.0);
        assert!(Style::Regular.font().glyph(b'A') != 0);
        assert!(Style::Regular.font().glyph(0x80) != 0, "the euro sign");
        assert_eq!(fit(Style::Regular, 10.0, "short", 100.0), "short");
        let cut = fit(
            Style::Regular,
            10.0,
            "a rather long product name indeed",
            60.0,
        );
        assert!(cut.ends_with('…') && width(Style::Regular, 10.0, &cut) <= 60.0);
        let lines = wrap(
            Style::Regular,
            10.0,
            "one two three four five six seven",
            50.0,
        );
        assert!(lines.len() > 2, "{lines:?}");
    }

    #[test]
    fn a_subset_keeps_the_glyphs_drawn() {
        let font = Style::Regular.font();
        let keep: BTreeSet<u16> = [font.glyph(b'A'), font.glyph(b'b')].into();
        let sub = font.subset(&keep);
        assert!(sub.len() < font.data.len() / 3, "{}", sub.len());
        assert_eq!(checksum(&sub), 0xB1B0_AFBA);
        // The table directory survives a re-parse.
        let leaked: &'static [u8] = Box::leak(sub.into_boxed_slice());
        let again = Font::parse(leaked, "x").unwrap();
        assert_eq!(again.width(b'A'), font.width(b'A'));
    }

    #[test]
    fn a_document_has_the_pdfa_parts() {
        let mut pdf = Pdf::new("Invoice (1)");
        pdf.page()
            .text(Style::Regular, 10.0, 50.0, 800.0, "Total (EUR) 1.00 €");
        let xml = b"<x/>";
        let out = pdf.finish(
            "2026-11-27",
            &[Attachment {
                name: "factur-x.xml",
                mime: "text/xml",
                description: "Invoice",
                relationship: "Alternative",
                data: xml,
            }],
            "",
        );
        let text = String::from_utf8_lossy(&out);
        assert!(text.starts_with("%PDF-1.7\n%"));
        for part in [
            "/Type /Catalog",
            "/OutputIntents",
            "/S /GTS_PDFA1",
            "<pdfaid:part>3</pdfaid:part>",
            "/AFRelationship /Alternative",
            "/Subtype /text#2Fxml",
            "/EmbeddedFiles << /Names [(factur-x.xml)",
            "/FontFile2",
            "/Encoding /WinAnsiEncoding",
            "/ID [<",
            "%%EOF",
        ] {
            assert!(text.contains(part), "{part}");
        }
        assert!(
            text.contains("(Total \\(EUR\\) 1.00 \\200) Tj"),
            "escaped text"
        );
        let profile = srgb_profile();
        assert_eq!(u32_at(&profile, 0) as usize, profile.len());
        assert_eq!(&profile[36..40], b"acsp");
    }
}
