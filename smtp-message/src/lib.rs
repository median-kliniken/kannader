use std::{
    net::{Ipv4Addr, Ipv6Addr},
    str,
};

use lazy_static::lazy_static;
use nom::{
    branch::alt,
    bytes::streaming::tag,
    combinator::{map, map_opt, opt, peek},
    multi::separated_nonempty_list,
    sequence::{pair, preceded, terminated},
    IResult,
};
use regex_automata::{Regex, RegexBuilder, DFA};

lazy_static! {
    static ref HOSTNAME_ASCII: Regex = RegexBuilder::new().anchored(true).build(
        r#"(?x)
            \[IPv6: [:.[:xdigit:]]+ \] |             # Ipv6
            \[ [.0-9]+ \] |                          # Ipv4
            [[:alnum:]] ([-[:alnum:]]* [[:alnum:]])? # Ascii-only domain
                ( \. [[:alnum:]] ([-[:alnum:]]* [[:alnum:]])? )*
        "#
    ).unwrap();

    static ref HOSTNAME_UTF8: Regex = RegexBuilder::new().anchored(true).build(
        r#"([-.[:alnum:]]|[[:^ascii:]])+"#
    ).unwrap();

    // Note: we have to disable the x flag here so that the # in the
    // middle of the character class does not get construed as a
    // comment
    static ref LOCALPART_ASCII: Regex = RegexBuilder::new().anchored(true).build(
        r#"(?x)
            " ( [[:ascii:]&&[^\\"[:cntrl:]]] |       # Quoted-string localpart
                \\ [[:ascii:]&&[:^cntrl:]] )* " |
            (?-x)[a-zA-Z0-9!#$%&'*+-/=?^_`{|}~]+(?x) # Dot-string localpart
                ( \. (?-x)[a-zA-Z0-9!#$%&'*+/=?^_`{|}~-]+(?x) )*
        "#
    ).unwrap();

    // Note: we have to disable the x flag here so that the # in the
    // middle of the character class does not get construed as a
    // comment
    static ref LOCALPART_UTF8: Regex = RegexBuilder::new().anchored(true).build(
        r#"(?x)
            " ( [^\\"[:cntrl:]] | \\ [[:^cntrl:]] )* " |                # Quoted-string localpart
            ( (?-x)[a-zA-Z0-9!#$%&'*+-/=?^_`{|}~](?x) | [[:^ascii:]] )+ # Dot-string localpart
                ( \. ( (?-x)[a-zA-Z0-9!#$%&'*+-/=?^_`{|}~](?x) | [[:^ascii:]] )+ )*
        "#
    ).unwrap();
}

// Implementation is similar to regex_automata's, but also returns the state
// when a match wasn't found
fn find_dfa<D: DFA>(dfa: &D, buf: &[u8]) -> Result<usize, D::ID> {
    let mut state = dfa.start_state();
    let mut last_match = if dfa.is_dead_state(state) {
        return Err(state);
    } else if dfa.is_match_state(state) {
        Some(0)
    } else {
        None
    };

    for (i, &b) in buf.iter().enumerate() {
        state = unsafe { dfa.next_state_unchecked(state, b) };
        if dfa.is_match_or_dead_state(state) {
            if dfa.is_dead_state(state) {
                return last_match.ok_or(state);
            }
            last_match = Some(i + 1);
        }
    }

    last_match.ok_or(state)
}

fn apply_regex<'a>(regex: &'a Regex) -> impl 'a + Fn(&[u8]) -> IResult<&[u8], &[u8]> {
    move |buf: &[u8]| {
        let dfa = regex.forward();

        let dfa_result = match dfa {
            regex_automata::DenseDFA::Standard(r) => find_dfa(r, buf),
            regex_automata::DenseDFA::ByteClass(r) => find_dfa(r, buf),
            regex_automata::DenseDFA::Premultiplied(r) => find_dfa(r, buf),
            regex_automata::DenseDFA::PremultipliedByteClass(r) => find_dfa(r, buf),
            other => find_dfa(other, buf),
        };

        match dfa_result {
            Ok(end) => Ok((&buf[end..], &buf[..end])),
            Err(s) if dfa.is_dead_state(s) => {
                Err(nom::Err::Error((buf, nom::error::ErrorKind::Verify)))
            }
            Err(_) => Err(nom::Err::Incomplete(nom::Needed::Unknown)),
        }
    }
}

fn maybe_terminator<'a>(terminator: &'a [u8]) -> impl 'a + Fn(&[u8]) -> IResult<&[u8], ()> {
    move |buf: &[u8]| {
        if terminator == b"" {
            Ok((buf, ()))
        } else {
            map(peek(tag(terminator)), |_| ())(buf)
        }
    }
}

// TODO: Ideally the ipv6 and ipv4 variants would be parsed in the single regex
// pass. However, that's hard to do, so let's just not do it for now and keep it
// as an optimization. So for now, it's just as well to return the parsed IPs,
// but some day they will probably be removed
/// Note: comparison happens only on the `raw` field, meaning that if you modify
/// or create a `Hostname` yourself it could have surprising results. But such a
/// `Hostname` would then not actually represent a real hostname, so you
/// probably would have had surprising results anyway.
#[derive(Debug, Eq)]
pub enum Hostname<S = String> {
    Utf8Domain { raw: S, punycode: String },
    AsciiDomain { raw: S },
    Ipv6 { raw: S, ip: Ipv6Addr },
    Ipv4 { raw: S, ip: Ipv4Addr },
}

impl<S> Hostname<S> {
    #[inline]
    pub fn parse<'a>(buf: &'a [u8]) -> IResult<&'a [u8], Hostname<S>>
    where
        S: From<&'a str>,
    {
        Self::parse_terminated(b"")(buf)
    }

    fn parse_terminated<'a, 'b>(
        term: &'b [u8],
    ) -> impl 'b + Fn(&'a [u8]) -> IResult<&'a [u8], Hostname<S>>
    where
        'a: 'b,
        S: 'b + From<&'a str>,
    {
        alt((
            map_opt(
                terminated(apply_regex(&HOSTNAME_ASCII), maybe_terminator(term)),
                |b: &[u8]| {
                    // The three below unsafe are OK, thanks to our
                    // regex validating that `b` is proper ascii
                    // (and thus utf-8)
                    let s = unsafe { str::from_utf8_unchecked(b) };

                    if b[0] != b'[' {
                        return Some(Hostname::AsciiDomain { raw: s.into() });
                    } else if b[1] == b'I' {
                        let ip = unsafe { str::from_utf8_unchecked(&b[6..b.len() - 1]) };
                        let ip = ip.parse::<Ipv6Addr>().ok()?;

                        return Some(Hostname::Ipv6 { raw: s.into(), ip });
                    } else {
                        let ip = unsafe { str::from_utf8_unchecked(&b[1..b.len() - 1]) };
                        let ip = ip.parse::<Ipv4Addr>().ok()?;

                        return Some(Hostname::Ipv4 { raw: s.into(), ip });
                    }
                },
            ),
            map_opt(
                terminated(apply_regex(&HOSTNAME_UTF8), maybe_terminator(term)),
                |res: &[u8]| {
                    // The below unsafe is OK, thanks to our regex
                    // never disabling the `u` flag and thus
                    // validating that the match is proper utf-8
                    let raw = unsafe { str::from_utf8_unchecked(res) };

                    // TODO: looks like idna exposes only an
                    // allocating method for validating an IDNA domain
                    // name. Maybe it'd be possible to get them to
                    // expose a validation-only function? Or maybe
                    // not.
                    let punycode = idna::Config::default()
                        .use_std3_ascii_rules(true)
                        .verify_dns_length(true)
                        .check_hyphens(true)
                        .to_ascii(raw)
                        .ok()?;

                    return Some(Hostname::Utf8Domain {
                        raw: raw.into(),
                        punycode,
                    });
                },
            ),
        ))
    }
}

impl<S> Hostname<S> {
    pub fn raw(&self) -> &S {
        match self {
            Hostname::Utf8Domain { raw, .. } => raw,
            Hostname::AsciiDomain { raw, .. } => raw,
            Hostname::Ipv4 { raw, .. } => raw,
            Hostname::Ipv6 { raw, .. } => raw,
        }
    }
}

impl<S: PartialEq> std::cmp::PartialEq for Hostname<S> {
    fn eq(&self, o: &Hostname<S>) -> bool {
        self.raw() == o.raw()
    }
}

#[cfg(test)]
impl<S: Eq + PartialEq> Hostname<S> {
    fn deep_equal(&self, o: &Hostname<S>) -> bool {
        match self {
            Hostname::Utf8Domain { raw, punycode } => match o {
                Hostname::Utf8Domain {
                    raw: raw2,
                    punycode: punycode2,
                } => raw == raw2 && punycode == punycode2,
                _ => false,
            },
            Hostname::AsciiDomain { raw } => match o {
                Hostname::AsciiDomain { raw: raw2 } => raw == raw2,
                _ => false,
            },
            Hostname::Ipv4 { raw, ip } => match o {
                Hostname::Ipv4 { raw: raw2, ip: ip2 } => raw == raw2 && ip == ip2,
                _ => false,
            },
            Hostname::Ipv6 { raw, ip } => match o {
                Hostname::Ipv6 { raw: raw2, ip: ip2 } => raw == raw2 && ip == ip2,
                _ => false,
            },
        }
    }
}

// TODO: consider adding `Sane` variant like OpenSMTPD does, that would not be
// matched by weird characters
#[derive(Debug, Eq, PartialEq)]
pub enum Localpart<S> {
    Ascii { raw: S },
    Quoted { raw: S },
    Utf8 { raw: S },
    QuotedUtf8 { raw: S },
}

impl<S> Localpart<S> {
    #[inline]
    pub fn parse<'a>(buf: &'a [u8]) -> IResult<&'a [u8], Localpart<S>>
    where
        S: From<&'a str>,
    {
        Self::parse_terminated(b"")(buf)
    }

    fn parse_terminated<'a, 'b>(
        term: &'b [u8],
    ) -> impl 'b + Fn(&'a [u8]) -> IResult<&'a [u8], Localpart<S>>
    where
        'a: 'b,
        S: 'b + From<&'a str>,
    {
        alt((
            map(
                terminated(apply_regex(&LOCALPART_ASCII), maybe_terminator(term)),
                |b: &[u8]| {
                    // The below unsafe is OK, thanks to our regex
                    // validating that `b` is proper ascii (and thus
                    // utf-8)
                    let s = unsafe { str::from_utf8_unchecked(b) };

                    if b[0] != b'"' {
                        return Localpart::Ascii { raw: s.into() };
                    } else {
                        return Localpart::Quoted { raw: s.into() };
                    }
                },
            ),
            map(
                terminated(apply_regex(&LOCALPART_UTF8), maybe_terminator(term)),
                |b: &[u8]| {
                    // The below unsafe is OK, thanks to our regex
                    // validating that `b` is proper utf-8 by never disabling the `u` flag
                    let s = unsafe { str::from_utf8_unchecked(b) };

                    if b[0] != b'"' {
                        return Localpart::Utf8 { raw: s.into() };
                    } else {
                        return Localpart::QuotedUtf8 { raw: s.into() };
                    }
                },
            ),
        ))
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct Email<S> {
    pub localpart: Localpart<S>,
    pub hostname: Option<Hostname<S>>,
}

impl<S> Email<S> {
    #[inline]
    pub fn parse<'a>(buf: &'a [u8]) -> IResult<&'a [u8], Email<S>>
    where
        S: From<&'a str>,
    {
        Self::parse_terminated(b"", b"")(buf)
    }

    // *IF* term != b"", then term_with_atsign must be term + b"@"
    #[inline]
    fn parse_terminated<'a, 'b>(
        term: &'b [u8],
        term_with_atsign: &'a [u8],
    ) -> impl 'b + Fn(&'a [u8]) -> IResult<&'a [u8], Email<S>>
    where
        'a: 'b,
        S: 'b + From<&'a str>,
    {
        map(
            pair(
                Localpart::parse_terminated(term_with_atsign),
                opt(preceded(tag(b"@"), Hostname::parse_terminated(term))),
            ),
            |(localpart, hostname)| Email {
                localpart,
                hostname,
            },
        )
    }
}

/// Note: for convenience this is not exactly like what is described by RFC5321,
/// and it does not contain the Email. Indeed, paths are *very* rare nowadays.
///
/// `Path` as defined here is what is specified in RFC5321 as `A-d-l`
#[derive(Debug, Eq, PartialEq)]
pub struct Path<S> {
    pub domains: Vec<Hostname<S>>,
}

impl<S> Path<S> {
    #[inline]
    pub fn parse<'a>(buf: &'a [u8]) -> IResult<&'a [u8], Path<S>>
    where
        S: From<&'a str>,
    {
        Self::parse_terminated(b"")(buf)
    }

    // *IF* you want a terminator, then term_with_comma must be term + b","
    #[inline]
    fn parse_terminated<'a, 'b>(
        term_with_comma: &'a [u8],
    ) -> impl 'b + Fn(&'a [u8]) -> IResult<&'a [u8], Path<S>>
    where
        'a: 'b,
        S: 'b + From<&'a str>,
    {
        map(
            separated_nonempty_list(
                tag(b","),
                preceded(tag(b"@"), Hostname::parse_terminated(term_with_comma)),
            ),
            |domains| Path { domains },
        )
    }
}

// TODO: add valid/incomplete/invalid tests for Path

fn email_in_path<'a, S>(buf: &'a [u8]) -> IResult<&'a [u8], (Option<Path<S>>, Email<S>)>
where
    S: From<&'a str>,
{
    pair(opt(terminated(Path::parse, tag(b":"))), Email::parse)(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    pub fn show_bytes(b: &[u8]) -> String {
        if let Ok(s) = str::from_utf8(b) {
            s.into()
        } else {
            format!("{:?}", b)
        }
    }

    #[test]
    fn hostname_valid() {
        let tests: &[(&[u8], &[u8], &[u8], Hostname<&str>)] = &[
            (b"foo--bar", b"", b"", Hostname::AsciiDomain {
                raw: "foo--bar",
            }),
            (b"foo.bar.baz", b"", b"", Hostname::AsciiDomain {
                raw: "foo.bar.baz",
            }),
            (b"1.2.3.4", b"", b"", Hostname::AsciiDomain {
                raw: "1.2.3.4",
            }),
            (b"[123.255.37.2]", b"", b"", Hostname::Ipv4 {
                raw: "[123.255.37.2]",
                ip: "123.255.37.2".parse().unwrap(),
            }),
            (b"[IPv6:0::ffff:8.7.6.5]", b"", b"", Hostname::Ipv6 {
                raw: "[IPv6:0::ffff:8.7.6.5]",
                ip: "0::ffff:8.7.6.5".parse().unwrap(),
            }),
            ("élégance.fr".as_bytes(), b"", b"", Hostname::Utf8Domain {
                raw: "élégance.fr",
                punycode: "xn--lgance-9uab.fr".into(),
            }),
            (b"foo.-bar.baz", b"", b".-bar.baz", Hostname::AsciiDomain {
                raw: "foo",
            }),
            (b"foo.bar.-baz", b"", b".-baz", Hostname::AsciiDomain {
                raw: "foo.bar",
            }),
            (
                "papier-maché.fr>".as_bytes(),
                b">",
                b">",
                Hostname::Utf8Domain {
                    raw: "papier-maché.fr",
                    punycode: "xn--papier-mach-lbb.fr".into(),
                },
            ),
        ];
        for (inp, terminate, rem, out) in tests {
            let parsed = Hostname::parse_terminated(*terminate)(inp);
            println!(
                "\nTest: {:?}\nParse result: {:?}\nExpected: {:?}",
                show_bytes(inp),
                parsed,
                out
            );
            match parsed {
                Ok((rest, host)) => assert!(rest == *rem && host.deep_equal(out)),
                x => panic!("Unexpected result: {:?}", x),
            }
        }
    }

    #[test]
    fn hostname_incomplete() {
        let tests: &[&[u8]] = &[b"[1.2", b"[IPv6:0::"];
        for inp in tests {
            let r = Hostname::<&str>::parse(inp);
            println!("{:?}:  {:?}", show_bytes(inp), r);
            assert!(r.unwrap_err().is_incomplete());
        }
    }

    #[test]
    fn hostname_invalid() {
        let tests: &[&[u8]] = &[
            b"-foo.bar",                 // No sub-domain starting with a dash
            b"\xFF",                     // No invalid utf-8
            "élégance.-fr".as_bytes(), // No dashes in utf-8 either
        ];
        for inp in tests {
            let r = Hostname::<String>::parse(inp);
            println!("{:?}: {:?}", show_bytes(inp), r);
            assert!(!r.unwrap_err().is_incomplete());
        }
    }

    #[test]
    fn localpart_valid() {
        let tests: &[(&[u8], &[u8], Localpart<&str>)] = &[
            (b"helloooo", b"", Localpart::Ascii { raw: "helloooo" }),
            (b"test.ing", b"", Localpart::Ascii { raw: "test.ing" }),
            (br#""hello""#, b"", Localpart::Quoted { raw: r#""hello""# }),
            (
                br#""hello world. This |$ a g#eat place to experiment !""#,
                b"",
                Localpart::Quoted {
                    raw: r#""hello world. This |$ a g#eat place to experiment !""#,
                },
            ),
            (
                br#""\"escapes\", useless like h\ere, except for quotes and backslashes\\""#,
                b"",
                Localpart::Quoted {
                    raw: r#""\"escapes\", useless like h\ere, except for quotes and backslashes\\""#,
                },
            ),
            // TODO: add Utf8 tests
        ];
        for (inp, rem, out) in tests {
            match Localpart::parse(inp) {
                Ok((rest, res)) if rest == *rem && res == *out => (),
                x => panic!("Unexpected result: {:?}", x),
            }
        }
    }

    // TODO: add incomplete and invalid localpart tests

    #[test]
    fn email_valid() {
        let tests: &[(&[u8], &[u8], Email<&str>)] = &[
            (b"t+e-s.t_i+n-g@foo.bar.baz", b"", Email {
                localpart: Localpart::Ascii {
                    raw: "t+e-s.t_i+n-g",
                },
                hostname: Some(Hostname::AsciiDomain { raw: "foo.bar.baz" }),
            }),
            (br#""quoted\"example"@example.org"#, b"", Email {
                localpart: Localpart::Quoted {
                    raw: r#""quoted\"example""#,
                },
                hostname: Some(Hostname::AsciiDomain { raw: "example.org" }),
            }),
            (b"postmaster>", b">", Email {
                localpart: Localpart::Ascii { raw: "postmaster" },
                hostname: None,
            }),
            (b"test>", b">", Email {
                localpart: Localpart::Ascii { raw: "test" },
                hostname: None,
            }),
        ];
        for (inp, rem, out) in tests {
            println!("Test: {:?}", show_bytes(inp));
            match Email::parse(inp) {
                Ok((rest, res)) if rest == *rem && res == *out => (),
                x => panic!("Unexpected result: {:?}", x),
            }
        }
    }

    // TODO: add incomplete email tests

    #[test]
    fn email_invalid() {
        let tests: &[&[u8]] = &[b"@foo.bar"];
        for inp in tests {
            let r = Email::<&str>::parse(inp);
            assert!(!r.unwrap_err().is_incomplete());
        }
    }

    #[test]
    fn email_in_path_valid() {
        let tests: &[(&[u8], &[u8], (Option<Path<&str>>, Email<&str>))] = &[
            (
                b"@foo.bar,@baz.quux:test@example.org",
                b"",
                (
                    Some(Path {
                        domains: vec![
                            Hostname::AsciiDomain { raw: "foo.bar" },
                            Hostname::AsciiDomain { raw: "baz.quux" },
                        ],
                    }),
                    Email {
                        localpart: Localpart::Ascii { raw: "test" },
                        hostname: Some(Hostname::AsciiDomain { raw: "example.org" }),
                    },
                ),
            ),
            (
                b"foo.bar@baz.quux",
                b"",
                (None, Email {
                    localpart: Localpart::Ascii { raw: "foo.bar" },
                    hostname: Some(Hostname::AsciiDomain { raw: "baz.quux" }),
                }),
            ),
        ];
        for (inp, rem, out) in tests {
            println!("Test: {:?}", show_bytes(inp));
            match email_in_path(inp) {
                Ok((rest, res)) if rest == *rem && res == *out => (),
                x => panic!("Unexpected result: {:?}", x),
            }
        }
    }
}
