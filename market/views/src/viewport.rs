use ratatui::{
    backend::{Backend, ClearType, WindowSize},
    buffer::Cell,
    layout::{Position, Size},
};

/// Hands Tickloop the keys a hosted Tick has no use for, so the user moves on from its card:
/// `ESC ] 7777 ; tickloop ; key=<name> BEL` on the Tick's terminal, after what it drew.
pub trait KeyBubbling {
    /// Tells the host the Tick had no use for `key`; nothing when no host listens.
    fn bubble(&mut self, key: &str) -> std::io::Result<()> {
        let _ = key;
        Ok(())
    }
}

/// Announces that the Tick hands the keys it has no use for to Tickloop.
pub const BUBBLING_ON: &str = "\x1b]7777;tickloop;bubble=on\x07";

pub fn bubbled_key(key: &str) -> String {
    format!("\x1b]7777;tickloop;key={key}\x07")
}

impl KeyBubbling for ratatui::backend::TestBackend {}

impl<B> KeyBubbling for ViewportBackend<B> {
    fn bubble(&mut self, key: &str) -> std::io::Result<()> {
        use std::io::Write;
        // Only a hosted Tick draws on a terminal of its own, which a host reads.
        let Some(tty) = &mut self.tty else {
            return Ok(());
        };
        tty.write_all(bubbled_key(key).as_bytes())?;
        tty.flush()
    }
}

pub struct ViewportBackend<B> {
    inner: B,
    maximum: Option<Size>,
    tty: Option<std::fs::File>,
}

impl<B> ViewportBackend<B> {
    pub fn new(inner: B, maximum: Option<Size>) -> Self {
        Self {
            inner,
            maximum,
            tty: None,
        }
    }
    pub fn with_tty(inner: B, maximum: Option<Size>, tty: std::fs::File) -> Self {
        Self {
            inner,
            maximum,
            tty: Some(tty),
        }
    }
    fn constrain(&self, size: Size) -> Size {
        self.maximum.map_or(size, |maximum| {
            Size::new(
                size.width.min(maximum.width),
                size.height.min(maximum.height),
            )
        })
    }

    fn tty_window_size(&self) -> std::io::Result<Option<WindowSize>> {
        use std::os::fd::AsRawFd;
        let Some(tty) = &self.tty else {
            return Ok(None);
        };
        let mut size: libc::winsize = unsafe { std::mem::zeroed() };
        if unsafe { libc::ioctl(tty.as_raw_fd(), libc::TIOCGWINSZ, &mut size) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Some(WindowSize {
            columns_rows: Size::new(size.ws_col.max(1), size.ws_row.max(1)),
            pixels: Size::new(size.ws_xpixel, size.ws_ypixel),
        }))
    }
}

impl<B: Backend> Backend for ViewportBackend<B>
where
    B::Error: From<std::io::Error>,
{
    type Error = B::Error;
    fn draw<'a, I>(&mut self, content: I) -> Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        self.inner.draw(content)
    }
    fn hide_cursor(&mut self) -> Result<(), Self::Error> {
        self.inner.hide_cursor()
    }
    fn show_cursor(&mut self) -> Result<(), Self::Error> {
        self.inner.show_cursor()
    }
    // Tick Commands run full-screen on the alternate screen with the cursor hidden, so the
    // position ratatui asks for before `Terminal::clear` carries no information. Answering
    // locally avoids the `ESC[6n` round trip, whose reply the patched crossterm input reader
    // does not deliver while an `EventStream` owns the terminal input.
    fn get_cursor_position(&mut self) -> Result<Position, Self::Error> {
        Ok(Position::ORIGIN)
    }
    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> Result<(), Self::Error> {
        self.inner.set_cursor_position(position)
    }
    fn clear(&mut self) -> Result<(), Self::Error> {
        self.inner.clear()
    }
    fn clear_region(&mut self, clear_type: ClearType) -> Result<(), Self::Error> {
        self.inner.clear_region(clear_type)
    }
    fn size(&self) -> Result<Size, Self::Error> {
        match self.tty_window_size()? {
            Some(size) => Ok(self.constrain(size.columns_rows)),
            None => self.inner.size().map(|size| self.constrain(size)),
        }
    }
    fn window_size(&mut self) -> Result<WindowSize, Self::Error> {
        let mut size = match self.tty_window_size()? {
            Some(size) => size,
            None => self.inner.window_size()?,
        };
        size.columns_rows = self.constrain(size.columns_rows);
        Ok(size)
    }
    fn flush(&mut self) -> Result<(), Self::Error> {
        self.inner.flush()
    }
}

impl<B: std::io::Write> std::io::Write for ViewportBackend<B> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.inner.write(bytes)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        std::io::Write::flush(&mut self.inner)
    }
}
