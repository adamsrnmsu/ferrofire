//! The text of an SCPI program message (IEEE 488.2 / SCPI-99), shared by the server and the client: split into program message
//! units at `;` outside quotes, a unit into header and parameters, the header into mnemonics, a parameter into number, keyword
//! or string. Pure functions, no state; an `Err(code)` is the SCPI error number the caller queues.

/// Split `s` at `sep` outside quotes (`"..."` or `'...'`, a doubled quote inside is a quote).
pub fn split(s: &str, sep: char) -> Vec<&str> {
    let (mut out, mut start, mut quote) = (vec![], 0, None);
    for (i, c) in s.char_indices() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c == sep => {
                out.push(&s[start..i]);
                start = i + c.len_utf8();
            }
            None => {}
        }
    }
    out.push(&s[start..]);
    out
}

/// A unit's header (up to the first white space) and its parameter text, trimmed.
pub fn header_rest(unit: &str) -> (&str, &str) {
    let u = unit.trim();
    match u.find(char::is_whitespace) {
        Some(i) => (&u[..i], u[i..].trim()),
        None => (u, ""),
    }
}

/// Whether the device answers this message: some unit's header ends in `?`. A `?` inside a string parameter does not count.
pub fn is_query(msg: &str) -> bool {
    split(msg, ';')
        .iter()
        .any(|u| header_rest(u).0.ends_with('?'))
}

/// A parsed header: `*IDN?` is common, `:SYST:ERR?` rooted, `MEAS:X` relative to the previous unit's path.
#[derive(Debug, PartialEq)]
pub struct Header {
    pub common: bool,
    pub rooted: bool,
    pub query: bool,
    /// The mnemonics, upper-cased, numeric suffix still attached.
    pub path: Vec<String>,
}

fn mnemonic(m: &str) -> bool {
    m.starts_with(|c: char| c.is_ascii_alphabetic())
        && m.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// `None` when the text is not a header at all (empty mnemonic, a character no mnemonic has, `*` with a path).
pub fn header(h: &str) -> Option<Header> {
    let (query, h) = h.strip_suffix('?').map_or((false, h), |r| (true, r));
    let (rooted, h) = h.strip_prefix(':').map_or((false, h), |r| (true, r));
    let (common, h) = h.strip_prefix('*').map_or((false, h), |r| (true, r));
    let path: Vec<String> = h.split(':').map(str::to_ascii_uppercase).collect();
    let ok = path.iter().all(|m| mnemonic(m)) && (!common || (path.len() == 1 && !rooted));
    ok.then_some(Header {
        common,
        rooted,
        query,
        path,
    })
}

#[derive(Debug, PartialEq)]
pub enum Param {
    Num(f64),
    /// Character data (`ON`, `MAX`), upper-cased.
    Word(String),
    Str(String),
}

/// The comma-separated parameters; `Err` is the SCPI error of the first one that does not parse.
pub fn params(rest: &str) -> Result<Vec<Param>, i32> {
    if rest.is_empty() {
        return Ok(vec![]);
    }
    split(rest, ',')
        .into_iter()
        .map(|t| param(t.trim()))
        .collect()
}

fn param(t: &str) -> Result<Param, i32> {
    match t.chars().next() {
        Some('"' | '\'') => string(t).map(Param::Str),
        Some(c) if c.is_ascii_digit() || matches!(c, '+' | '-' | '.') => number(t).map(Param::Num),
        Some(c) if c.is_ascii_alphabetic() => word(t),
        Some('#') => Err(-104),
        _ => Err(-102),
    }
}

fn word(t: &str) -> Result<Param, i32> {
    match t.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        true => Ok(Param::Word(t.to_ascii_uppercase())),
        false => Err(-102),
    }
}

/// A quoted string whose closing quote is the last character; `""` inside is one quote.
fn string(t: &str) -> Result<String, i32> {
    let q = t.chars().next().ok_or(-102)?;
    let (mut out, mut rest) = (String::new(), t[1..].chars().peekable());
    while let Some(c) = rest.next() {
        match (c == q, rest.peek() == Some(&q)) {
            (false, _) => out.push(c),
            (true, true) => {
                out.push(q);
                rest.next();
            }
            (true, false) => return rest.next().map_or(Ok(out), |_| Err(-102)),
        }
    }
    Err(-102)
}

fn digits(b: &[u8], i: usize) -> usize {
    b[i..].iter().take_while(|c| c.is_ascii_digit()).count()
}

/// The length of the NR1 / NR2 / NR3 number at the start of `b`: `[sign] digits [. digits] [E [sign] digits]`, 0 if there is none.
fn number_len(b: &[u8]) -> usize {
    let mut i = usize::from(matches!(b.first(), Some(b'+' | b'-')));
    let int = digits(b, i);
    i += int;
    let frac = if b.get(i) == Some(&b'.') {
        digits(b, i + 1)
    } else {
        0
    };
    if int + frac == 0 {
        return 0;
    }
    i += if b.get(i) == Some(&b'.') { 1 + frac } else { 0 };
    if matches!(b.get(i), Some(b'e' | b'E')) {
        let j = i + 1 + usize::from(matches!(b.get(i + 1), Some(b'+' | b'-')));
        let exp = digits(b, j);
        if exp > 0 {
            return j + exp;
        }
    }
    i
}

/// -121 invalid character in number, -131 invalid suffix (`5V`), -123 exponent too large.
fn number(t: &str) -> Result<f64, i32> {
    let n = number_len(t.as_bytes());
    match t[n..].trim_start().chars().next() {
        _ if n == 0 => Err(-121),
        Some(c) if c.is_ascii_alphabetic() => Err(-131),
        Some(_) => Err(-121),
        None => t.parse::<f64>().ok().filter(|x| x.is_finite()).ok_or(-123),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn units_split_at_semicolons_outside_quotes() {
        assert_eq!(split("A;B", ';'), ["A", "B"]);
        assert_eq!(split("A \"x;y\";B 'p;q'", ';'), ["A \"x;y\"", "B 'p;q'"]);
        assert_eq!(split("A \"x\"\";\"y;B", ';'), ["A \"x\"\";\"y", "B"]);
        assert_eq!(split("A \"x;B", ';'), ["A \"x;B"]);
        assert_eq!(split("", ';'), [""]);
        assert_eq!(split(";", ';'), ["", ""]);
        assert_eq!(split("é;ü", ';'), ["é", "ü"]);
    }

    #[test]
    fn header_and_parameters_are_cut_at_white_space() {
        assert_eq!(header_rest("  SOUR:X \t 1, 2 "), ("SOUR:X", "1, 2"));
        assert_eq!(header_rest("*IDN?"), ("*IDN?", ""));
        assert_eq!(header_rest(""), ("", ""));
    }

    #[test]
    fn a_message_is_a_query_when_a_header_ends_in_a_question_mark() {
        assert!(is_query("*IDN?"));
        assert!(is_query("*CLS;*IDN?"));
        assert!(is_query("*IDN?;*CLS"));
        assert!(!is_query("*CLS"));
        assert!(!is_query("DISP:TEXT \"ok?\""));
        assert!(!is_query("A 'x;y?'"));
        assert!(!is_query(""));
    }

    fn h(path: &[&str], common: bool, rooted: bool, query: bool) -> Option<Header> {
        Some(Header {
            common,
            rooted,
            query,
            path: path.iter().map(|s| s.to_string()).collect(),
        })
    }

    #[test]
    fn headers_are_upper_cased_with_their_flags() {
        assert_eq!(header("*idn?"), h(&["IDN"], true, false, true));
        assert_eq!(header("*ESE"), h(&["ESE"], true, false, false));
        assert_eq!(header(":syst:err?"), h(&["SYST", "ERR"], false, true, true));
        assert_eq!(
            header("Meas2:a_b"),
            h(&["MEAS2", "A_B"], false, false, false)
        );
        for bad in [
            "", "?", ":", "A::B", "A:", "1A", "*", "*A:B", ":*A", "A-B", "A.B", "É",
        ] {
            assert_eq!(header(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn parameters_are_numbers_keywords_and_strings() {
        use Param::*;
        let w = |s: &str| Word(s.into());
        assert_eq!(params(""), Ok(vec![]));
        assert_eq!(
            params("1, -2.5 ,+.5, 5., 1E3, 1.5e-2, MAX, on , \"a,b\", 'it''s'"),
            Ok(vec![
                Num(1.0),
                Num(-2.5),
                Num(0.5),
                Num(5.0),
                Num(1000.0),
                Num(0.015),
                w("MAX"),
                w("ON"),
                Str("a,b".into()),
                Str("it's".into())
            ])
        );
        assert_eq!(
            params("\"say \"\"hi\"\"\""),
            Ok(vec![Str("say \"hi\"".into())])
        );
    }

    #[test]
    fn a_parameter_that_does_not_parse_names_its_scpi_error() {
        for (text, code) in [
            ("1,,2", -102),
            ("1,", -102),
            ("\"abc", -102),
            ("\"a\"b", -102),
            ("a-b", -102),
            ("@", -102),
            ("#H1F", -104),
            ("-", -121),
            ("+", -121),
            (".", -121),
            ("1.2.3", -121),
            ("1e", -131),
            ("5V", -131),
            ("5 V", -131),
            ("1e999", -123),
            ("-1e999", -123),
            ("--1", -121),
        ] {
            assert_eq!(params(text), Err(code), "{text:?}");
        }
        assert_eq!(params("inf"), Ok(vec![Param::Word("INF".into())]));
    }

    #[test]
    fn number_len_is_the_longest_number_prefix() {
        for (s, n) in [
            ("12", 2),
            ("-1.5", 4),
            (".5", 2),
            ("5.", 2),
            ("1e5", 3),
            ("1e+5x", 4),
            ("1e", 1),
            ("1e+", 1),
            ("1.e2", 4),
            ("", 0),
            ("e5", 0),
            (".e5", 0),
            ("+", 0),
            ("a", 0),
        ] {
            assert_eq!(number_len(s.as_bytes()), n, "{s:?}");
        }
    }
}
