use std::fmt;

use helpers::*;

macro_rules! alpha_lower { () => ("abcdefghijklmnopqrstuvwxyz") }
macro_rules! alpha_upper { () => ("ABCDEFGHIJKLMNOPQRSTUVWXYZ") }
macro_rules! alpha       { () => (concat!(alpha_lower!(), alpha_upper!())) }
macro_rules! digit       { () => ("0123456789") }
macro_rules! alnum       { () => (concat!(alpha!(), digit!())) }
macro_rules! atext       { () => (concat!(alnum!(), "!#$%&'*+-/=?^_`{|}~")) }

// TODO: strip return-path in MAIL FROM, like OpenSMTPD does, in order to not be thrown out by mail
// systems like orange's, maybe?

#[cfg_attr(test, derive(PartialEq))]
#[derive(Copy, Clone)]
pub struct Email<'a> {
    localpart: &'a [u8],
    hostname: &'a [u8],
}

impl<'a> Email<'a> {
    pub fn raw_localpart(&self) -> &[u8] {
        self.localpart
    }

    // Note: this may contain unexpected characters, check RFC5321 / RFC5322 for details
    // This is a canonicalized version of the potentially quoted localpart, not designed to be
    // sent over the wire as it is no longer correctly quoted
    pub fn localpart(&self) -> Vec<u8> {
        if self.localpart[0] != b'"' {
            self.localpart.to_owned()
        } else {
            #[derive(Copy, Clone)]
            enum State { Start, Backslash }

            let mut res = self.localpart.iter().skip(1).scan(State::Start, |state, &x| {
                match (*state, x) {
                    (State::Backslash, _) => { *state = State::Start;     Some(Some(x)) },
                    (_, b'\\')            => { *state = State::Backslash; Some(None   ) },
                    (_, _)                => { *state = State::Start;     Some(Some(x)) },
                }
            }).filter_map(|x| x).collect::<Vec<u8>>();
            assert_eq!(res.pop().unwrap(), b'"');
            res
        }
    }

    pub fn raw_hostname(&self) -> &[u8] {
        self.hostname
    }
}

impl<'a> fmt::Debug for Email<'a> {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        write!(f, "Email {{ localpart: {}, hostname: {} }}",
               bytes_to_dbg(self.localpart), bytes_to_dbg(self.hostname))
    }
}

named!(pub hostname(&[u8]) -> &[u8],
    alt!(
        recognize!(preceded!(tag!("["), take_until_and_consume!("]"))) |
        recognize!(separated_list_complete!(tag!("."), is_a!(concat!(alnum!(), "-"))))
    )
);

named!(dot_string(&[u8]) -> &[u8], recognize!(
    separated_list!(tag!("."), is_a!(atext!()))
));

// See RFC 5321 § 4.1.2
named!(quoted_string(&[u8]) -> &[u8], recognize!(do_parse!(
    tag!("\"") >>
    many0!(alt!(
        preceded!(tag!("\\"), verify!(take!(1), |x: &[u8]| 32 <= x[0] && x[0] <= 126)) |
        verify!(take!(1), |x: &[u8]| 32 <= x[0] && x[0] != 34 && x[0] != 92 && x[0] <= 126)
    )) >>
    tag!("\"") >>
    ()
)));

named!(localpart(&[u8]) -> &[u8], alt!(quoted_string | dot_string));

named!(email(&[u8]) -> Email, do_parse!(
    local: localpart >>
    tag!("@") >>
    host: hostname >>
    (Email {
        localpart: local,
        hostname: host,
    })
));

named!(address_in_path(&[u8]) -> Email, do_parse!(
    opt!(do_parse!(
        separated_list!(tag!(","), do_parse!(tag!("@") >> hostname >> ())) >>
        tag!(":") >>
        ()
    )) >>
    res: email >>
    (res)
));

named!(pub address_in_maybe_bracketed_path(&[u8]) -> Email,
    alt!(
        do_parse!(
            tag!("<") >>
            addr: address_in_path >>
            tag!(">") >>
            (addr)
        ) |
        address_in_path
    )
);

named!(pub postmaster_maybe_bracketed_address(&[u8]) -> Email,
    alt!(
        map!(tag_no_case!("<postmaster>"), |x| Email {
            localpart: &x[1..(x.len() - 1)],
            hostname: b"",
        }) |
        map!(tag_no_case!("postmaster"), |x| Email {
            localpart: x,
            hostname: b"",
        })
    )
);

named!(pub full_maybe_bracketed_path(&[u8]) -> &[u8], recognize!(address_in_maybe_bracketed_path));

named!(pub eat_spaces, eat_separator!(" \t"));

#[cfg(test)]
mod tests {
    use nom::*;
    use parse_helpers::*;

    #[test]
    fn valid_hostnames() {
        let tests = &[
            &b"foo--bar"[..],
            &b"foo.bar.baz"[..],
            &b"1.2.3.4"[..],
            &b"[123.255.37.2]"[..],
            &b"[IPv6:0::ffff:8.7.6.5]"[..],
        ];
        for test in tests {
            assert_eq!(hostname(test), IResult::Done(&b""[..], *test));
        }
    }

    #[test]
    fn valid_dot_strings() {
        let tests: &[&[u8]] = &[
            // Adding an '@' so that tests do not return Incomplete
            b"helloooo@",
            b"test.ing@",
        ];
        for test in tests {
            assert_eq!(dot_string(test), IResult::Done(&b"@"[..], &test[0..test.len()-1]));
        }
    }

    #[test]
    fn valid_quoted_strings() {
        let tests: &[&[u8]] = &[
            br#""hello""#,
            br#""hello world. This |$ a g#eat place to experiment !""#,
            br#""\"escapes\", useless like h\ere, except for quotes and \\backslashes""#,
        ];
        for test in tests {
            assert_eq!(quoted_string(test), IResult::Done(&b""[..], *test));
        }
    }

    #[test]
    fn valid_emails() {
        let tests: Vec<(&[u8], Email)> = vec![
            (b"t+e-s.t_i+n-g@foo.bar.baz", Email {
                localpart: b"t+e-s.t_i+n-g",
                hostname: b"foo.bar.baz",
            }),
            (br#""quoted\"example"@example.org"#, Email {
                localpart: br#""quoted\"example""#,
                hostname: b"example.org",
            }),
        ];
        for (s, r) in tests.into_iter() {
            assert_eq!(email(s), IResult::Done(&b""[..], r));
        }
    }

    #[test]
    fn nice_localpart() {
        let tests: Vec<(&[u8], &[u8])> = vec![
            (b"t+e-s.t_i+n-g@foo.bar.baz", b"t+e-s.t_i+n-g"),
            (br#""quoted\"example"@example.org"#, br#"quoted"example"#),
            (br#""escaped\\exa\mple"@example.org"#, br#"escaped\example"#),
        ];
        for (s, r) in tests {
            assert_eq!(email(s).unwrap().1.localpart(), r);
        }
    }

    #[test]
    fn valid_addresses_in_paths() {
        let tests = &[
            (&b"@foo.bar,@baz.quux:test@example.org"[..], Email {
                localpart: b"test",
                hostname: b"example.org",
            }),
            (&b"foo.bar@baz.quux"[..], Email {
                localpart: b"foo.bar",
                hostname: b"baz.quux",
            }),
        ];
        for test in tests {
            assert_eq!(address_in_path(test.0), IResult::Done(&b""[..], test.1));
        }
    }

    #[test]
    fn valid_addresses_in_maybe_bracketed_paths() {
        let tests = &[
            (&b"@foo.bar,@baz.quux:test@example.org"[..], Email {
                localpart: b"test",
                hostname: b"example.org",
            }),
            (&b"<@foo.bar,@baz.quux:test@example.org>"[..], Email {
                localpart: b"test",
                hostname: b"example.org",
            }),
            (&b"<foo@bar.baz>"[..], Email {
                localpart: b"foo",
                hostname: b"bar.baz",
            }),
            (&b"foo@bar.baz"[..], Email {
                localpart: b"foo",
                hostname: b"bar.baz",
            }),
        ];
        for test in tests {
            assert_eq!(address_in_maybe_bracketed_path(test.0), IResult::Done(&b""[..], test.1));
        }
    }

    #[test]
    fn valid_full_maybe_bracketed_paths() {
        let tests = &[
            &b"@foo.bar,@baz.quux:test@example.org"[..],
            &b"<@foo.bar,@baz.quux:test@example.org>"[..],
            &b"foo@bar.baz"[..],
            &b"<foo@bar.baz>"[..],
        ];
        for test in tests {
            assert_eq!(full_maybe_bracketed_path(test), IResult::Done(&b""[..], *test));
        }
    }
}
