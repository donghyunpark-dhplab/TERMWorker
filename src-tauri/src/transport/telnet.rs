//! Telnet protocol filter (RFC 854) with option negotiation using the
//! Q-method from RFC 1143 so negotiation can never loop.
//!
//! Supported options: BINARY (0), ECHO (1, server side), SGA (3),
//! TERMINAL-TYPE (24) and NAWS (31, window size). Everything else is refused.

pub const IAC: u8 = 255;
pub const DONT: u8 = 254;
pub const DO: u8 = 253;
pub const WONT: u8 = 252;
pub const WILL: u8 = 251;
pub const SB: u8 = 250;
pub const SE: u8 = 240;

pub const OPT_BINARY: u8 = 0;
pub const OPT_ECHO: u8 = 1;
pub const OPT_SGA: u8 = 3;
pub const OPT_TTYPE: u8 = 24;
pub const OPT_NAWS: u8 = 31;

const TTYPE_IS: u8 = 0;
const TTYPE_SEND: u8 = 1;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Q {
    No,
    Yes,
    WantYes,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Data,
    Iac,
    Verb(u8),
    Sb,
    SbIac,
}

pub struct Telnet {
    state: State,
    sb: Vec<u8>,
    /// Options we perform (negotiated with WILL/WONT).
    us: [Q; 256],
    /// Options the server performs (negotiated with DO/DONT).
    him: [Q; 256],
    cols: u16,
    rows: u16,
    term_type: String,
    last_was_cr: bool,
    /// The server refused BINARY: it follows NVT rules and sends a bare CR as "CR NUL".
    /// Servers that ignore the request entirely (busybox telnetd) send raw bytes instead.
    nvt_server: bool,
}

impl Telnet {
    pub fn new(cols: u16, rows: u16) -> Self {
        Self {
            state: State::Data,
            sb: Vec::new(),
            us: [Q::No; 256],
            him: [Q::No; 256],
            cols,
            rows,
            term_type: "XTERM-256COLOR".into(),
            last_was_cr: false,
            nvt_server: false,
        }
    }

    /// Negotiation we start ourselves right after connecting.
    pub fn initial(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        // BINARY both ways: 8-bit clean, so file transfers (ZMODEM) pass unchanged.
        for opt in [OPT_NAWS, OPT_TTYPE, OPT_SGA, OPT_BINARY] {
            self.us[opt as usize] = Q::WantYes;
            out.extend_from_slice(&[IAC, WILL, opt]);
        }
        for opt in [OPT_ECHO, OPT_SGA, OPT_BINARY] {
            self.him[opt as usize] = Q::WantYes;
            out.extend_from_slice(&[IAC, DO, opt]);
        }
        out
    }

    #[allow(dead_code)] // will drive automatic local echo
    pub fn server_echoes(&self) -> bool {
        self.him[OPT_ECHO as usize] == Q::Yes
    }

    /// Split incoming bytes into terminal data and protocol replies that must be sent back.
    pub fn receive(&mut self, input: &[u8], data: &mut Vec<u8>, reply: &mut Vec<u8>) {
        for &b in input {
            self.state = match self.state {
                State::Data => {
                    if b == IAC {
                        State::Iac
                    } else {
                        let binary = self.him[OPT_BINARY as usize] == Q::Yes;
                        // NVT: "CR NUL" means a bare carriage return. Only servers that
                        // follow NVT add the NUL; from others it is data (in a file transfer).
                        if !(b == 0 && self.last_was_cr && !binary && self.nvt_server) {
                            data.push(b);
                        }
                        self.last_was_cr = b == b'\r';
                        State::Data
                    }
                }
                State::Iac => match b {
                    IAC => {
                        data.push(IAC);
                        self.last_was_cr = false;
                        State::Data
                    }
                    WILL | WONT | DO | DONT => State::Verb(b),
                    SB => {
                        self.sb.clear();
                        State::Sb
                    }
                    // NOP, GA, AYT, etc. carry no data for us.
                    _ => State::Data,
                },
                State::Verb(verb) => {
                    self.negotiate(verb, b, reply);
                    State::Data
                }
                State::Sb => {
                    if b == IAC {
                        State::SbIac
                    } else {
                        if self.sb.len() < 4096 {
                            self.sb.push(b);
                        }
                        State::Sb
                    }
                }
                State::SbIac => match b {
                    SE => {
                        self.subnegotiation(reply);
                        State::Data
                    }
                    IAC => {
                        self.sb.push(IAC);
                        State::Sb
                    }
                    _ => State::Data,
                },
            };
        }
    }

    /// Prepare user input for the wire: escape IAC and turn bare CR into CR NUL.
    pub fn encode_outgoing(&self, input: &[u8]) -> Vec<u8> {
        let binary = self.us[OPT_BINARY as usize] == Q::Yes;
        let mut out = Vec::with_capacity(input.len() + 4);
        for (i, &b) in input.iter().enumerate() {
            match b {
                IAC => out.extend_from_slice(&[IAC, IAC]),
                b'\r' if !binary && input.get(i + 1) != Some(&b'\n') => out.extend_from_slice(&[b'\r', 0]),
                _ => out.push(b),
            }
        }
        out
    }

    /// Prepare file-transfer data for the wire. Outside BINARY mode the server drops
    /// the byte after a CR if it is LF or NUL ("CR LF" and "CR NUL" both mean CR), so
    /// such a CR is sent as "CR NUL" and the server hands on the original bytes. Other
    /// CRs go out bare: busybox telnetd looks for the NUL only within one read, so
    /// every extra NUL is a chance of one leaking into the data.
    pub fn encode_data(&self, input: &[u8]) -> Vec<u8> {
        let binary = self.us[OPT_BINARY as usize] == Q::Yes;
        let mut out = Vec::with_capacity(input.len() + input.len() / 64 + 4);
        for (i, &b) in input.iter().enumerate() {
            match b {
                IAC => out.extend_from_slice(&[IAC, IAC]),
                // The last byte: the next one (in a later write) may be LF or NUL.
                b'\r' if !binary && matches!(input.get(i + 1), None | Some(b'\n') | Some(0)) => {
                    out.extend_from_slice(&[b'\r', 0])
                }
                _ => out.push(b),
            }
        }
        out
    }

    /// Window size changed. Returns a NAWS update if the server asked for it.
    pub fn resize(&mut self, cols: u16, rows: u16) -> Option<Vec<u8>> {
        self.cols = cols;
        self.rows = rows;
        (self.us[OPT_NAWS as usize] == Q::Yes).then(|| self.naws())
    }

    fn naws(&self) -> Vec<u8> {
        let mut out = vec![IAC, SB, OPT_NAWS];
        for v in [self.cols, self.rows] {
            for byte in v.to_be_bytes() {
                out.push(byte);
                if byte == IAC {
                    out.push(IAC);
                }
            }
        }
        out.extend_from_slice(&[IAC, SE]);
        out
    }

    fn we_support(opt: u8) -> bool {
        matches!(opt, OPT_BINARY | OPT_SGA | OPT_TTYPE | OPT_NAWS)
    }

    fn we_accept_from_server(opt: u8) -> bool {
        matches!(opt, OPT_BINARY | OPT_ECHO | OPT_SGA)
    }

    fn negotiate(&mut self, verb: u8, opt: u8, reply: &mut Vec<u8>) {
        let i = opt as usize;
        match verb {
            DO => match self.us[i] {
                Q::No if Self::we_support(opt) => {
                    self.us[i] = Q::Yes;
                    reply.extend_from_slice(&[IAC, WILL, opt]);
                    self.enabled_us(opt, reply);
                }
                Q::No => reply.extend_from_slice(&[IAC, WONT, opt]),
                Q::WantYes => {
                    self.us[i] = Q::Yes;
                    self.enabled_us(opt, reply);
                }
                Q::Yes => {}
            },
            DONT => match self.us[i] {
                Q::Yes => {
                    self.us[i] = Q::No;
                    reply.extend_from_slice(&[IAC, WONT, opt]);
                }
                Q::WantYes => self.us[i] = Q::No,
                Q::No => {}
            },
            WILL => match self.him[i] {
                Q::No if Self::we_accept_from_server(opt) => {
                    self.him[i] = Q::Yes;
                    reply.extend_from_slice(&[IAC, DO, opt]);
                }
                Q::No => reply.extend_from_slice(&[IAC, DONT, opt]),
                Q::WantYes => self.him[i] = Q::Yes,
                Q::Yes => {}
            },
            WONT => match self.him[i] {
                Q::Yes => {
                    self.him[i] = Q::No;
                    reply.extend_from_slice(&[IAC, DONT, opt]);
                    if opt == OPT_BINARY {
                        self.nvt_server = true;
                    }
                }
                Q::WantYes => {
                    self.him[i] = Q::No;
                    if opt == OPT_BINARY {
                        self.nvt_server = true;
                    }
                }
                Q::No => {}
            },
            _ => {}
        }
    }

    fn enabled_us(&mut self, opt: u8, reply: &mut Vec<u8>) {
        if opt == OPT_NAWS {
            reply.extend_from_slice(&self.naws());
        }
    }

    fn subnegotiation(&mut self, reply: &mut Vec<u8>) {
        if self.sb.len() >= 2 && self.sb[0] == OPT_TTYPE && self.sb[1] == TTYPE_SEND {
            reply.extend_from_slice(&[IAC, SB, OPT_TTYPE, TTYPE_IS]);
            reply.extend_from_slice(self.term_type.as_bytes());
            reply.extend_from_slice(&[IAC, SE]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(t: &mut Telnet, input: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let (mut d, mut r) = (Vec::new(), Vec::new());
        t.receive(input, &mut d, &mut r);
        (d, r)
    }

    #[test]
    fn passes_plain_data_and_unescapes_iac() {
        let mut t = Telnet::new(80, 24);
        let (d, r) = feed(&mut t, b"login: \xff\xffok");
        assert_eq!(d, b"login: \xffok");
        assert!(r.is_empty());
    }

    #[test]
    fn answers_requests_for_our_initial_offers_without_echoing_back() {
        let mut t = Telnet::new(120, 40);
        let init = t.initial();
        assert!(init.windows(3).any(|w| w == [IAC, WILL, OPT_NAWS]));
        // Server agrees to NAWS: we must send the size, not another WILL.
        let (_, r) = feed(&mut t, &[IAC, DO, OPT_NAWS]);
        assert_eq!(r, vec![IAC, SB, OPT_NAWS, 0, 120, 0, 40, IAC, SE]);
        // Server agrees to echo: no reply needed.
        let (_, r) = feed(&mut t, &[IAC, WILL, OPT_ECHO]);
        assert!(r.is_empty());
        assert!(t.server_echoes());
    }

    #[test]
    fn refuses_unknown_options() {
        let mut t = Telnet::new(80, 24);
        let (_, r) = feed(&mut t, &[IAC, DO, 39 /* NEW-ENVIRON */, IAC, WILL, 5 /* STATUS */]);
        assert_eq!(r, vec![IAC, WONT, 39, IAC, DONT, 5]);
    }

    #[test]
    fn replies_to_terminal_type_query_split_across_reads() {
        let mut t = Telnet::new(80, 24);
        feed(&mut t, &[IAC, DO, OPT_TTYPE]);
        let (_, r1) = feed(&mut t, &[IAC, SB, OPT_TTYPE]);
        let (_, r2) = feed(&mut t, &[TTYPE_SEND, IAC, SE]);
        assert!(r1.is_empty());
        let mut expected = vec![IAC, SB, OPT_TTYPE, TTYPE_IS];
        expected.extend_from_slice(b"XTERM-256COLOR");
        expected.extend_from_slice(&[IAC, SE]);
        assert_eq!(r2, expected);
    }

    #[test]
    fn naws_escapes_255() {
        let mut t = Telnet::new(80, 24);
        feed(&mut t, &[IAC, DO, OPT_NAWS]);
        assert_eq!(t.resize(255, 50).unwrap(), vec![IAC, SB, OPT_NAWS, 0, 255, 255, 0, 50, IAC, SE]);
    }

    #[test]
    fn outgoing_escaping() {
        let t = Telnet::new(80, 24);
        assert_eq!(t.encode_outgoing(b"a\xffb\r"), b"a\xff\xffb\r\0");
        assert_eq!(t.encode_outgoing(b"ls\r\n"), b"ls\r\n");
    }

    #[test]
    fn strips_nul_after_cr_from_an_nvt_server() {
        let mut t = Telnet::new(80, 24);
        t.initial();
        feed(&mut t, &[IAC, WONT, OPT_BINARY]);
        let (d, _) = feed(&mut t, b"a\r");
        let (d2, _) = feed(&mut t, b"\0b");
        assert_eq!([d, d2].concat(), b"a\rb");
    }

    #[test]
    fn keeps_cr_nul_from_a_server_that_ignores_binary() {
        // busybox telnetd never answers option requests and sends pty output raw;
        // "0D 00" inside ZMODEM data must survive.
        let mut t = Telnet::new(80, 24);
        t.initial();
        let (d, _) = feed(&mut t, b"a\r\0b");
        assert_eq!(d, b"a\r\0b");
    }

    #[test]
    fn asks_for_binary_and_uses_it() {
        let mut t = Telnet::new(80, 24);
        let init = t.initial();
        assert!(init.windows(3).any(|w| w == [IAC, WILL, OPT_BINARY]));
        assert!(init.windows(3).any(|w| w == [IAC, DO, OPT_BINARY]));
        let (_, r) = feed(&mut t, &[IAC, DO, OPT_BINARY, IAC, WILL, OPT_BINARY]);
        assert!(r.is_empty(), "agreement needs no reply: {r:?}");
        let (d, _) = feed(&mut t, b"\r\0");
        assert_eq!(d, b"\r\0");
        assert_eq!(t.encode_data(b"\r\n\xff"), b"\r\n\xff\xff");
        assert_eq!(t.encode_outgoing(b"ls\r"), b"ls\r");
    }

    #[test]
    fn transfer_data_survives_a_non_binary_server() {
        let t = Telnet::new(80, 24);
        // The server removes the NUL after each CR, restoring "\r\n", "\r\0" and "\r".
        assert_eq!(t.encode_data(b"\r\n\r\0\rA\xff\r"), b"\r\0\n\r\0\0\rA\xff\xff\r\0");
    }
}
