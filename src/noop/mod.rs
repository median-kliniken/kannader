use std::fmt;

use helpers::*;
use parse_helpers::*;

#[cfg_attr(test, derive(PartialEq))]
pub struct NoopCommand<'a> {
    string: &'a [u8],
}

impl<'a> NoopCommand<'a> {
    pub fn string(&self) -> &'a [u8] {
        self.string
    }
}

impl<'a> fmt::Debug for NoopCommand<'a> {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        write!(f, "NoopCommand {{ string: {} }}", bytes_to_dbg(self.string))
    }
}

named!(pub command_noop_args(&[u8]) -> NoopCommand, do_parse!(
    eat_spaces >>
    res: take_until!("\r\n") >>
    tag!("\r\n") >>
    (NoopCommand {
        string: res,
    })
));

#[cfg(test)]
mod tests {
    use super::*;
    use nom::*;

    #[test]
    fn valid_command_noop_args() {
        let tests = vec![
            (&b" \t hello.world \t \r\n"[..], NoopCommand {
                string: &b"hello.world \t "[..],
            }),
            (&b"\r\n"[..], NoopCommand {
                string: &b""[..],
            }),
            (&b" \r\n"[..], NoopCommand {
                string: &b""[..],
            }),
        ];
        for (s, r) in tests.into_iter() {
            assert_eq!(command_noop_args(s), IResult::Done(&b""[..], r));
        }
    }
}
