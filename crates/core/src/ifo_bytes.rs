//! Parse a StarDict `.ifo` file from an in-memory byte slice.
//!
//! This mirrors `stardict` 0.2.3's `Ifo::new(PathBuf)` exactly, but reads from
//! bytes so a front-end can parse dictionary metadata without touching the
//! filesystem.

use std::io::{BufRead, BufReader};

use stardict::ifo::{Ifo, Version};

use crate::Error;

fn invalid_data(message: impl Into<String>) -> Error {
    Error::Stardict(Box::new(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        message.into(),
    )))
}

/// Parse a StarDict `.ifo` file from `data`.
///
/// The returned [`Ifo`] is identical to what
/// [`stardict::ifo::Ifo::new`] produces for a path containing exactly these
/// bytes.
pub fn parse_ifo(data: &[u8]) -> Result<Ifo, Error> {
    let mut ifo = Ifo {
        version: Version::V242,
        bookname: String::new(),
        wordcount: 0,
        synwordcount: 0,
        idxfilesize: 0,
        idxoffsetbits: 32,
        author: String::new(),
        email: String::new(),
        website: String::new(),
        description: String::new(),
        date: String::new(),
        sametypesequence: String::new(),
        dicttype: String::new(),
    };

    for line in BufReader::new(data).lines() {
        let line = line.map_err(|e| Error::Stardict(Box::new(e)))?;
        let Some(id) = line.find('=') else {
            continue;
        };
        let key = &line[..id];
        let val = String::from(&line[id + 1..]);
        match key {
            "version" => match val.as_str() {
                "2.4.2" => {}
                "3.0.0" => ifo.version = Version::V300,
                _ => return Err(invalid_data(format!("invalid version: {val}"))),
            },
            "bookname" => ifo.bookname = val,
            "wordcount" => {
                ifo.wordcount = val
                    .parse()
                    .map_err(|_| invalid_data("invalid value for wordcount in ifo"))?
            }
            "synwordcount" => {
                ifo.synwordcount = val
                    .parse()
                    .map_err(|_| invalid_data("invalid value for synwordcount in ifo"))?
            }
            "idxfilesize" => {
                ifo.idxfilesize = val
                    .parse()
                    .map_err(|_| invalid_data("invalid value for idxfilesize in ifo"))?
            }
            "idxoffsetbits" => {
                ifo.idxoffsetbits = val
                    .parse()
                    .map_err(|_| invalid_data("invalid value for idxoffsetbits in ifo"))?
            }
            "author" => ifo.author = val,
            "email" => ifo.email = val,
            "website" => ifo.website = val,
            "description" => ifo.description = val,
            "date" => ifo.date = val,
            "sametypesequence" => ifo.sametypesequence = val,
            "dicttype" => ifo.dicttype = val,
            _ => (),
        }
    }

    Ok(ifo)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_gcide_ifo_identically() {
        const IFO: &str = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/gcide/dictd_www.dict.org_gcide.ifo"
        );

        let data = std::fs::read(IFO).expect("read gcide ifo");
        let parsed = parse_ifo(&data).expect("parse gcide ifo");
        let expected =
            stardict::Ifo::new(std::path::PathBuf::from(IFO)).expect("stardict parse gcide ifo");

        assert!(matches!(parsed.version, Version::V242));
        assert!(matches!(expected.version, Version::V242));
        assert_eq!(parsed.bookname, expected.bookname);
        assert_eq!(parsed.wordcount, expected.wordcount);
        assert_eq!(parsed.synwordcount, expected.synwordcount);
        assert_eq!(parsed.idxfilesize, expected.idxfilesize);
        assert_eq!(parsed.idxoffsetbits, expected.idxoffsetbits);
        assert_eq!(parsed.author, expected.author);
        assert_eq!(parsed.email, expected.email);
        assert_eq!(parsed.website, expected.website);
        assert_eq!(parsed.description, expected.description);
        assert_eq!(parsed.date, expected.date);
        assert_eq!(parsed.sametypesequence, expected.sametypesequence);
        assert_eq!(parsed.dicttype, expected.dicttype);
    }
}
