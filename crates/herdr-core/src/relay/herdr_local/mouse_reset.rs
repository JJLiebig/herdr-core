use super::{ESC, Relay, Screen, pass};
use crate::runtime::{Core, State};

const RESET: &[u8] = b"\x1b[?9;1000;1002;1003;1005;1006;1015;1016l";

#[derive(Default)]
pub(super) struct MouseReset {
    /// The root PID for which we last observed a different foreground process.
    away: Option<u32>,
    pub(super) pending: bool,
    /// An ESC at a possible cancellation boundary, waiting for its next byte.
    pub(super) held_escape: bool,
}

impl MouseReset {
    fn returned(&mut self, root: u32, foreground: i32) -> bool {
        if foreground <= 0 {
            return false;
        }
        if foreground as u32 == root {
            self.away.take() == Some(root)
        } else {
            self.away = Some(root);
            false
        }
    }
}

impl Core {
    /// The holder queues this observation under the same lock as PTY output.
    pub(crate) fn mouse_foreground(&self, st: &mut State, id: u32, instance: &str, foreground: i32) {
        let Some(terminal) = st.reg.terminals.get(&id).filter(|t| t.is_live() && t.holder.as_deref() == Some(instance))
        else {
            return;
        };
        let Some(root) = terminal.pid else { return };
        let reset = &mut st.relays.entry(id).or_default().mouse_reset;
        if reset.returned(root, foreground)
            && let Some(shell) = crate::foreground::about(foreground)
            && crate::platform::mouse_root_is_shell(&shell, &terminal.argv)
        {
            reset.pending = true;
            self.output(st, id, instance, &[]);
        }
    }
}

impl Relay {
    fn write_mouse_reset(&mut self, screen: &mut Screen, out: &mut Vec<u8>) {
        pass(screen, out, RESET);
        self.mouse_reset.pending = false;
        self.mouse_reset.held_escape = false;
    }

    pub(super) fn hold_mouse_escape(&mut self) -> bool {
        if self.mouse_reset.pending {
            self.held.push(ESC);
            self.mouse_reset.held_escape = true;
            return true;
        }
        false
    }

    /// ESC followed by backslash can finish a string. Other escapes replace
    /// the old sequence, so the reset can use that same cancellation point.
    pub(super) fn resume_mouse_escape(&mut self, screen: &mut Screen, out: &mut Vec<u8>, next: u8) {
        self.mouse_reset.held_escape = false;
        if next != b'\\' {
            let mut previous = std::mem::take(&mut self.held);
            previous.pop(); // The replacing ESC is processed after the reset.
            pass(screen, out, &previous);
            self.write_mouse_reset(screen, out);
            self.held.push(ESC);
        }
    }

    /// Recovering viewers need the first safe paint even if this read also
    /// starts a new sequence. Stop at ground or at a replacement escape.
    pub(super) fn ground_for_recovery(&mut self, screen: &mut Screen, bytes: &mut Vec<u8>) -> Option<(usize, Vec<u8>)> {
        let limit = if self.mouse_reset.pending {
            bytes
                .iter()
                .enumerate()
                .position(|(i, &b)| b == ESC && bytes.get(i + 1) != Some(&b'\\'))
                .unwrap_or(bytes.len())
        } else {
            bytes.len()
        };
        if let Some(n) = screen.write_until_ground(&bytes[..limit]) {
            return Some((n, bytes[..n].to_vec()));
        }
        if limit == bytes.len() {
            self.mouse_reset.held_escape = false;
            return None;
        }
        if limit + 1 == bytes.len() {
            self.hold_mouse_escape();
            bytes.truncate(limit);
            return None;
        }
        let mut prefix = bytes[..limit].to_vec();
        self.write_mouse_reset(screen, &mut prefix);
        Some((limit, prefix))
    }

    /// Called between sequences buffered by the relay. Ghostty may still be
    /// finishing a sequence or a UTF-8 character that spans reads.
    pub(super) fn pass_mouse_boundary(&mut self, screen: &mut Screen, out: &mut Vec<u8>, mut bytes: &[u8]) {
        if self.mouse_reset.pending {
            let Some(n) = screen.write_until_ground(bytes) else {
                out.extend_from_slice(bytes);
                return;
            };
            out.extend_from_slice(&bytes[..n]);
            self.write_mouse_reset(screen, out);
            bytes = &bytes[n..];
        }
        pass(screen, out, bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::Relay;
    use ghostty_vt::Terminal;

    #[test]
    fn returning_to_the_shell_clears_mouse_modes_in_live_output_and_new_paints() {
        let mut relay = Relay::default();
        let mut screen = Terminal::new(80, 24, 1 << 20);
        let mut attached = Terminal::new(80, 24, 1 << 20);
        let feed = |relay: &mut Relay, screen: &mut Terminal, attached: &mut Terminal, bytes: &[u8]| {
            attached.write(&relay.feed(screen, bytes));
        };
        let foreground = |relay: &mut Relay, pid| {
            if relay.mouse_reset.returned(10, pid) {
                relay.mouse_reset.pending = true;
            }
        };
        let modes = [9, 1000, 1002, 1003, 1005, 1006, 1015, 1016];

        foreground(&mut relay, 10);
        foreground(&mut relay, 11);
        feed(&mut relay, &mut screen, &mut attached, b"\x1b[?1049h\x1b[?9;1000;1002;1003;1005;1006;1015;1016h");
        assert!(modes.iter().all(|&mode| attached.dec_mode(mode)));
        foreground(&mut relay, 12);
        feed(&mut relay, &mut screen, &mut attached, b"\x1b[?2004");
        assert!(attached.dec_mode(1003), "another child keeps the mouse");
        foreground(&mut relay, -1);
        foreground(&mut relay, 10);
        feed(&mut relay, &mut screen, &mut attached, b"");
        assert!(attached.dec_mode(1003), "a pending sequence must finish before the reset");

        let (out, recovered) = relay.feed_to_ground(&mut screen, b"h");
        attached.write(&out);
        let (at, paint) = recovered.unwrap();
        let mut repainted = Terminal::new(80, 24, 1 << 20);
        repainted.write(&paint);
        repainted.write(&out[at..]);
        let mut reattached = Terminal::new(80, 24, 1 << 20);
        reattached.write(&screen.paint(0));
        for view in [&screen, &attached, &repainted, &reattached] {
            assert!(modes.iter().all(|&mode| !view.dec_mode(mode)));
            assert!(view.alt_screen());
            assert!(view.dec_mode(2004), "the reset cannot tear a bracketed paste sequence");
        }

        // Later PTY output can enable the mouse before the next foreground sample.
        feed(&mut relay, &mut screen, &mut attached, b"\x1b[?1003;1006h");
        assert!(attached.dec_mode(1003) && attached.dec_mode(1006));

        for (pid, next) in [(13, b"\x1b[?1003h".as_slice()), (14, b"\x18\x1b[?1003h".as_slice())] {
            foreground(&mut relay, pid);
            feed(&mut relay, &mut screen, &mut attached, b"\x1b[");
            foreground(&mut relay, 10);
            feed(&mut relay, &mut screen, &mut attached, next);
            assert!(screen.dec_mode(1003) && attached.dec_mode(1003), "replacement sequence must win");
            assert!(!screen.dec_mode(1006) && !attached.dec_mode(1006), "the old encoding is gone");
        }
    }

    #[test]
    fn replacement_escapes_precede_new_modes_and_string_terminators_keep_their_effects() {
        let long_dcs = [b"\x1bPq".as_slice(), &[b'?'; 600]].concat();
        for (prefix, recovering) in [(b"\x1b(".as_slice(), false), (long_dcs.as_slice(), true)] {
            let mut relay = Relay::default();
            let mut screen = Terminal::new(80, 24, 1 << 20);
            let mut view = Terminal::new(80, 24, 1 << 20);
            view.write(&relay.feed(&mut screen, b"\x1b[?1003;1006h"));
            view.write(&relay.feed(&mut screen, prefix));
            relay.mouse_reset.pending = true;

            if recovering {
                let (out, paint) = relay.feed_to_ground(&mut screen, b"\x1b");
                assert!(paint.is_none());
                view.write(&out);
                let (out, paint) = relay.feed_to_ground(&mut screen, b"[?1003h");
                view.write(&out);
                let (at, paint) = paint.unwrap();
                let mut recovered = Terminal::new(80, 24, 1 << 20);
                recovered.write(&paint);
                recovered.write(&out[at..]);
                assert!(recovered.dec_mode(1003) && !recovered.dec_mode(1006));
            } else {
                view.write(&relay.feed(&mut screen, b"\x1b"));
                view.write(&relay.feed(&mut screen, b"[?1003h"));
            }
            assert!(screen.dec_mode(1003) && view.dec_mode(1003));
            assert!(!screen.dec_mode(1006) && !view.dec_mode(1006));
        }

        let mut relay = Relay::default();
        let mut screen = Terminal::new(80, 24, 1 << 20);
        relay.feed(&mut screen, b"\x1b[?1003h\x1b]2;whole title");
        relay.mouse_reset.pending = true;
        relay.feed(&mut screen, b"\x1b");
        relay.feed(&mut screen, b"\\");
        assert_eq!(screen.title(), "whole title");
        assert!(!screen.dec_mode(1003));
    }
}
