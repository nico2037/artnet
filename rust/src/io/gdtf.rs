//! GDTF loading on top of the `gdtf` crate.
//!
//! The crate requires `DMXChannel/@Highlight`, which the GDTF spec makes optional (default "None"),
//! and rejects most real-world files without it. Such files are repacked in memory with the default added.

use std::borrow::Cow;
use std::io::{Cursor, Read, Write};

use anyhow::{anyhow, Context, Result};
use gdtf::GdtfFile;

const DESCRIPTION: &str = "description.xml";

pub fn load(bytes: Vec<u8>) -> Result<GdtfFile> {
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).context("GDTF is no zip archive")?;
    let mut xml = String::new();
    zip.by_name(DESCRIPTION).context("GDTF without description.xml")?.read_to_string(&mut xml)?;
    let bytes = match add_missing_highlight(&xml) {
        Cow::Borrowed(_) => zip.into_inner().into_inner(),
        Cow::Owned(fixed) => repack(&mut zip, &fixed)?,
    };
    GdtfFile::new(Cursor::new(bytes)).map_err(|e| anyhow!("parsing GDTF description: {e:?}"))
}

/// Copy of the archive with a new description.xml; resources are copied unchanged.
fn repack(zip: &mut zip::ZipArchive<Cursor<Vec<u8>>>, description: &str) -> Result<Vec<u8>> {
    let mut out = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let stored = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().to_string();
        out.start_file(name.as_str(), stored)?;
        if name == DESCRIPTION {
            out.write_all(description.as_bytes())?;
        } else {
            std::io::copy(&mut entry, &mut out)?;
        }
    }
    Ok(out.finish()?.into_inner())
}

/// Adds `Highlight="None"` to every `<DMXChannel …>` start tag that has no Highlight attribute.
fn add_missing_highlight(xml: &str) -> Cow<'_, str> {
    const TAG: &str = "<DMXChannel";
    let mut out = String::new();
    let mut copied = 0;
    let mut search = 0;
    while let Some(found) = xml[search..].find(TAG) {
        let start = search + found;
        let after_name = start + TAG.len();
        search = after_name;
        // <DMXChannels is a different element
        if !xml[after_name..].starts_with(|c: char| c.is_whitespace() || c == '>' || c == '/') {
            continue;
        }
        let end = tag_end(xml, after_name);
        if has_attribute(&xml[after_name..end], "Highlight") {
            continue;
        }
        out.push_str(&xml[copied..after_name]);
        out.push_str(" Highlight=\"None\"");
        copied = after_name;
    }
    if copied == 0 {
        return Cow::Borrowed(xml);
    }
    out.push_str(&xml[copied..]);
    Cow::Owned(out)
}

/// Index of the `>` closing a start tag, skipping quoted attribute values.
fn tag_end(xml: &str, from: usize) -> usize {
    let mut quote = None;
    for (i, c) in xml[from..].char_indices() {
        match (quote, c) {
            (None, '"' | '\'') => quote = Some(c),
            (Some(q), c) if c == q => quote = None,
            (None, '>') => return from + i,
            _ => {}
        }
    }
    xml.len()
}

fn has_attribute(attributes: &str, name: &str) -> bool {
    attributes
        .match_indices(name)
        .any(|(i, _)| attributes[..i].ends_with(char::is_whitespace) && attributes[i + name.len()..].trim_start().starts_with('='))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlight_added_only_where_missing() {
        let xml = r#"<DMXChannels><DMXChannel DMXBreak="1" Offset="1"><X/></DMXChannel><DMXChannel Highlight="255/1" Geometry="a>b"/><DMXChannel/></DMXChannels>"#;
        assert_eq!(
            add_missing_highlight(xml),
            r#"<DMXChannels><DMXChannel Highlight="None" DMXBreak="1" Offset="1"><X/></DMXChannel><DMXChannel Highlight="255/1" Geometry="a>b"/><DMXChannel Highlight="None"/></DMXChannels>"#
        );
        let complete = r#"<DMXChannel Highlight="None"/>"#;
        assert!(matches!(add_missing_highlight(complete), Cow::Borrowed(_)));
    }

    #[test]
    fn attribute_names_match_whole_words() {
        assert!(has_attribute(r#" Offset="1" Highlight = "None""#, "Highlight"));
        assert!(!has_attribute(r#" MyHighlight="1""#, "Highlight"));
        assert!(!has_attribute(r#" Name="Highlight""#, "Highlight"));
    }
}
