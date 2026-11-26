//! CSV as spreadsheets write it (RFC 4180): quoted fields with `""` for a
//! quote, line breaks inside quotes, CRLF or LF, a UTF-8 byte order mark.
//! A file whose header has more `;` than `,` is read with `;`, as
//! spreadsheets in much of Europe save it.

use std::fmt;

/// Why a CSV file could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CsvError(pub String);

impl fmt::Display for CsvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for CsvError {}

/// A CSV file: its header and rows. Every row has a cell per header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

impl Table {
    /// Reads `text`: the first line names the columns (trimmed, lowercase).
    /// Blank lines are skipped; a short row is padded with empty cells.
    ///
    /// # Errors
    ///
    /// When there is no header, a header repeats, a quote is never closed
    /// or a row has more cells than the header.
    pub fn parse(text: &str) -> Result<Self, CsvError> {
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let delimiter = delimiter(text);
        let mut records = records(text, delimiter)?.into_iter();
        let headers: Vec<String> = records
            .next()
            .ok_or_else(|| CsvError("the file is empty".into()))?
            .into_iter()
            .map(|h| h.trim().to_lowercase())
            .collect();
        for (i, h) in headers.iter().enumerate() {
            if !h.is_empty() && headers[..i].contains(h) {
                return Err(CsvError(format!("the column `{h}` appears twice")));
            }
        }
        let mut rows = Vec::new();
        for (n, mut r) in records.enumerate() {
            if r.iter().all(|c| c.trim().is_empty()) {
                continue;
            }
            if r.len() > headers.len() {
                return Err(CsvError(format!(
                    "line {} has {} cells, the header {}",
                    n + 2,
                    r.len(),
                    headers.len()
                )));
            }
            r.resize(headers.len(), String::new());
            rows.push(r);
        }
        Ok(Self { headers, rows })
    }

    /// The column named `name`, if the file has it.
    pub fn column(&self, name: &str) -> Option<usize> {
        self.headers.iter().position(|h| h == name)
    }

    /// Row `row`'s cell in column `name`, trimmed; empty when absent.
    pub fn get(&self, row: usize, name: &str) -> &str {
        self.column(name)
            .and_then(|c| self.rows.get(row)?.get(c))
            .map_or("", |s| s.trim())
    }
}

/// `;` when the first line has more of them than commas, outside quotes.
fn delimiter(text: &str) -> char {
    let (mut commas, mut semicolons, mut quoted) = (0, 0, false);
    for c in text.chars() {
        match c {
            '"' => quoted = !quoted,
            ',' if !quoted => commas += 1,
            ';' if !quoted => semicolons += 1,
            '\n' if !quoted => break,
            _ => {}
        }
    }
    if semicolons > commas {
        ';'
    } else {
        ','
    }
}

fn records(text: &str, delimiter: char) -> Result<Vec<Vec<String>>, CsvError> {
    let mut out = Vec::new();
    let mut row = Vec::new();
    let mut cell = String::new();
    let mut chars = text.chars().peekable();
    let mut quoted = false;
    let mut line = 1;
    let mut opened_at = 0;
    while let Some(c) = chars.next() {
        if quoted {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    cell.push('"');
                }
                '"' => quoted = false,
                '\n' => {
                    line += 1;
                    cell.push('\n');
                }
                '\r' if chars.peek() == Some(&'\n') => {}
                c => cell.push(c),
            }
            continue;
        }
        match c {
            '"' if cell.trim().is_empty() => {
                cell.clear();
                quoted = true;
                opened_at = line;
            }
            c if c == delimiter => row.push(std::mem::take(&mut cell)),
            '\r' if chars.peek() == Some(&'\n') => {}
            '\n' | '\r' => {
                line += 1;
                row.push(std::mem::take(&mut cell));
                out.push(std::mem::take(&mut row));
            }
            c => cell.push(c),
        }
    }
    if quoted {
        return Err(CsvError(format!(
            "a quote opened on line {opened_at} is never closed"
        )));
    }
    if !cell.is_empty() || !row.is_empty() {
        row.push(cell);
        out.push(row);
    }
    Ok(out)
}

/// `rows` (the header first) as CSV with CRLF line ends. A cell that a
/// spreadsheet would run as a formula (starting with `=`, `+`, `-`, `@`, a
/// tab or a carriage return, and not a number) gets a leading `'`.
pub fn write(rows: &[Vec<String>]) -> String {
    let mut out = String::new();
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            let formula =
                cell.starts_with(['=', '+', '-', '@', '\t', '\r']) && cell.parse::<f64>().is_err();
            let cell = if formula {
                format!("'{cell}")
            } else {
                cell.clone()
            };
            if cell.contains([',', ';', '"', '\n', '\r'])
                || cell.starts_with(' ')
                || cell.ends_with(' ')
            {
                out.push('"');
                out.push_str(&cell.replace('"', "\"\""));
                out.push('"');
            } else {
                out.push_str(&cell);
            }
        }
        out.push_str("\r\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(cells: &[&str]) -> Vec<String> {
        cells.iter().map(|c| (*c).to_owned()).collect()
    }

    #[test]
    fn reads_what_spreadsheets_write() {
        let t = Table::parse(
            "\u{feff}Handle, Title ,Price\r\ntee,\"Tee, \"\"organic\"\"\",29.00\r\n\r\nmug,\"Two\r\nlines\",\n",
        )
        .unwrap();
        assert_eq!(t.headers, row(&["handle", "title", "price"]));
        assert_eq!(t.rows.len(), 2);
        assert_eq!(t.get(0, "title"), "Tee, \"organic\"");
        assert_eq!(t.get(1, "title"), "Two\nlines");
        assert_eq!(t.get(1, "price"), "");
        assert_eq!(t.get(0, "missing"), "");
    }

    #[test]
    fn reads_semicolons_and_short_rows() {
        let t = Table::parse("sku;price;note\nA;1,50\nB;2,00;\"x;y\"").unwrap();
        assert_eq!(
            t.rows,
            vec![row(&["A", "1,50", ""]), row(&["B", "2,00", "x;y"])]
        );
    }

    #[test]
    fn says_what_is_wrong() {
        assert!(Table::parse("").is_err());
        assert_eq!(
            Table::parse("a,b\n1,2,3").unwrap_err().0,
            "line 2 has 3 cells, the header 2"
        );
        assert_eq!(
            Table::parse("a,b\n\"open,2").unwrap_err().0,
            "a quote opened on line 2 is never closed"
        );
        assert!(Table::parse("a,A").unwrap_err().0.contains("twice"));
    }

    #[test]
    fn writes_what_it_reads() {
        let rows = vec![
            row(&["handle", "title", "price"]),
            row(&["tee", "Tee, \"organic\"", "-5"]),
            row(&["mug", "Two\nlines", " padded "]),
        ];
        let text = write(&rows);
        assert!(text.ends_with("\r\n"));
        let t = Table::parse(&text).unwrap();
        assert_eq!(t.rows, rows[1..]);
    }

    #[test]
    fn formulas_do_not_run() {
        let text = write(&[row(&["=HYPERLINK(\"x\")", "+1", "-1.5", "@sum"])]);
        assert_eq!(text, "\"'=HYPERLINK(\"\"x\"\")\",+1,-1.5,'@sum\r\n");
    }
}
