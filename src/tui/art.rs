//! Cover art, drawn as pixels rather than as text.
//!
//! Crunchyroll hangs a poster off every series and a still off every episode, and a
//! terminal that speaks kitty, sixel or iTerm2 can draw them properly. Everything here is
//! best-effort: a CDN that will not answer, a JPEG that will not decode - none of it is
//! worth a word on the status line, let alone taking the catalogue away from someone who
//! only wanted to browse. A panel with no picture in it is simply left to the caller to
//! fill with something else. A terminal with no graphics protocol at all still gets the
//! pictures, as half-blocks: see [`Setting::Auto`].

use std::borrow::Borrow;
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::Hash;
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

use image::DynamicImage;
use ratatui::Frame;
use ratatui::layout::{Rect, Size};
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::protocol::Protocol;
use ratatui_image::{FilterType, Image, Resize};
use serde::Deserialize;

/// How long a fetch of one image may take from start to finish. A poster is a few dozen
/// kilobytes off a CDN; anything past this is a connection that has stopped moving.
const FETCH_TIMEOUT: Duration = Duration::from_secs(20);

/// How many fetches run at once. Enough that a wall of covers coming into view fills in
/// a row at a time rather than a tile at a time, few enough that scrolling a page does
/// not open a hundred sockets.
const FETCHERS: usize = 5;

/// How many decoded images are kept. A screenful of covers is a few dozen posters, and
/// the cache has to hold that screen, the row either side of it that a keypress brings
/// in, and the poster and still the columns draw - or scrolling back up one row fetches
/// again what was on screen a second ago. Not the whole page, though: a hundred series
/// at most of a megabyte each once they are pixels is a hundred megabytes of artwork
/// nobody is looking at.
const DECODED_CACHE: usize = 96;

/// How many encoded images are kept: one per picture per panel size. Every tile on the
/// wall of covers is one of these, so this has to hold a screenful of them as well, or
/// each frame would re-encode the tiles the last frame pushed out.
const ENCODED_CACHE: usize = 96;

/// How many pictures may wait for a fetcher. Scrolling a long wall asks for every
/// screenful it passes, and without a limit the screen it stops on would wait behind all
/// of them. The oldest are let go past this - they were on a screen that has gone by -
/// and asked for again should they come back into view.
const WAITING: usize = 64;

/// Whether the artwork is drawn.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum Setting {
    /// Drawn with the best the terminal can manage: kitty, sixel or iTerm2 where it
    /// speaks one of them, and half-blocks where it speaks none.
    ///
    /// This used to leave half-blocks off, on the grounds that a mosaic of coloured cells
    /// is not much of a picture and drags a hundred colours of its own across the
    /// colourscheme. That was the right call while the artwork was decoration beside the
    /// lists. The wall of covers made the posters the way a catalogue is browsed, and a
    /// wall of empty frames in every terminal that is not kitty is a worse answer than a
    /// rough picture - someone who disagrees has `off`, and `i`.
    #[default]
    Auto,
    /// Drawn with whatever the terminal can manage, half-blocks included - which is now
    /// what `auto` does as well. Kept so that a config file or a script that says `on`
    /// goes on meaning what it always meant.
    On,
    Off,
}

/// A map that forgets its oldest entry once it is full.
///
/// Not quite a least-recently-used cache - an entry looked at again does not move back to
/// the front - which for a list being scrolled through amounts to the same thing and is a
/// good deal less code.
struct Ring<K, V> {
    entries: HashMap<K, V>,
    order: VecDeque<K>,
    capacity: usize,
}

impl<K: Clone + Eq + Hash, V> Ring<K, V> {
    fn new(capacity: usize) -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            capacity,
        }
    }

    /// Borrowed lookup, so a `String` key can be asked for with a `&str` rather than
    /// with a copy of it made for the question and thrown away with the answer.
    fn get<Q>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.entries.get(key)
    }

    fn insert(&mut self, key: K, value: V) {
        if self.entries.insert(key.clone(), value).is_none() {
            self.order.push_back(key);
        }
        while self.order.len() > self.capacity {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            }
        }
    }
}

/// The pictures waiting for a fetcher, oldest first, and who is there to fetch them.
///
/// A stack rather than a queue: the newest request is for whatever is on screen now, and
/// that is the one worth having first. A channel would serve them in the order they were
/// asked for, which after a fast scroll is the order of screens nobody is looking at any
/// more.
#[derive(Default)]
struct Waiting {
    urls: VecDeque<String>,
    /// How many fetchers are still running. None at all - a test's gallery, or every
    /// thread having failed to start - means nothing asked for will ever arrive.
    fetchers: usize,
    /// Set as the gallery goes, so the fetchers stop waiting for work.
    closed: bool,
}

impl Waiting {
    /// Adds `url` on top, and hands back whatever fell off the bottom to make room.
    fn push(&mut self, url: String) -> Vec<String> {
        self.urls.push_back(url);
        let over = self.urls.len().saturating_sub(WAITING);
        self.urls.drain(..over).collect()
    }

    /// The newest request.
    fn pop(&mut self) -> Option<String> {
        self.urls.pop_back()
    }
}

/// The waiting pictures, and the bell a fetcher sleeps on until there are some.
#[derive(Default)]
struct Wants {
    waiting: Mutex<Waiting>,
    ready: Condvar,
}

impl Wants {
    /// The next picture to fetch, waiting for one if there is none; `None` once the
    /// gallery has gone.
    fn next(&self) -> Option<String> {
        let mut waiting = self.waiting.lock().expect("artwork queue poisoned");
        loop {
            if waiting.closed {
                return None;
            }
            if let Some(url) = waiting.pop() {
                return Some(url);
            }
            waiting = self.ready.wait(waiting).expect("artwork queue poisoned");
        }
    }
}

/// A decoded image, or the knowledge that this URL will never produce one. A failure is
/// worth remembering: without it the same broken poster is fetched again on every frame
/// the cursor rests on it.
type Decoded = Option<DynamicImage>;

/// The pictures, and the terminal's ability to draw them.
pub struct Gallery {
    picker: Picker,
    enabled: bool,
    wants: Arc<Wants>,
    arrived: Receiver<(String, Decoded)>,
    decoded: Ring<String, Decoded>,
    encoded: Ring<(String, u16, u16), Option<Protocol>>,
    /// URLs a fetcher is working on, so a cursor resting on a series does not ask for the
    /// same poster ten times a second.
    asked: HashSet<String>,
}

impl Gallery {
    /// Asks the terminal what it can draw and sets up the fetchers.
    ///
    /// This has to run after the alternate screen is entered and before any key is read:
    /// the question is escape sequences written to stdout, the answer comes back off
    /// stdin, and an answer read as a keypress is an answer lost.
    pub fn open(setting: Setting) -> Self {
        // A terminal that will not answer the query still gets halfblocks, which is the
        // same thing `from_query_stdio` falls back to on its own.
        let picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());
        let enabled = match setting {
            Setting::Auto | Setting::On => true,
            Setting::Off => false,
        };

        let wants = Arc::new(Wants::default());
        let (outbox, arrived) = channel::<(String, Decoded)>();
        wants
            .waiting
            .lock()
            .expect("artwork queue poisoned")
            .fetchers = FETCHERS;
        // Threads end when the gallery closes the queue on its way out, and a fetch still
        // in flight then finishes into a closed channel rather than holding up the quit.
        for _ in 0..FETCHERS {
            let wants = Arc::clone(&wants);
            let outbox = outbox.clone();
            thread::spawn(move || {
                if let Ok(client) = reqwest::blocking::Client::builder()
                    .timeout(FETCH_TIMEOUT)
                    .build()
                {
                    while let Some(url) = wants.next() {
                        let picture = fetch(&client, &url);
                        if outbox.send((url, picture)).is_err() {
                            break;
                        }
                    }
                }
                if let Ok(mut waiting) = wants.waiting.lock() {
                    waiting.fetchers = waiting.fetchers.saturating_sub(1);
                }
            });
        }

        Self {
            picker,
            enabled,
            wants,
            arrived,
            decoded: Ring::new(DECODED_CACHE),
            encoded: Ring::new(ENCODED_CACHE),
            asked: HashSet::new(),
        }
    }

    /// A gallery with nothing behind it: no fetchers, and only whatever a test puts into
    /// it by hand. Querying the terminal from a test would write escape sequences at
    /// whatever is running the suite and then wait for an answer nobody is going to give.
    #[cfg(test)]
    pub fn detached(enabled: bool) -> Self {
        let (_, arrived) = channel::<(String, Decoded)>();
        Self {
            picker: Picker::halfblocks(),
            enabled,
            wants: Arc::new(Wants::default()),
            arrived,
            decoded: Ring::new(DECODED_CACHE),
            encoded: Ring::new(ENCODED_CACHE),
            asked: HashSet::new(),
        }
    }

    /// Puts a picture in as though it had been fetched.
    #[cfg(test)]
    pub fn preload(&mut self, url: &str, picture: DynamicImage) {
        self.decoded.insert(url.to_owned(), Some(picture));
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// What the terminal answered when it was asked what it can draw. It was asked once,
    /// at startup, and mpv has to be told the same thing when it takes the screen over -
    /// so the answer is worth handing out rather than asking for again.
    pub fn protocol(&self) -> ProtocolType {
        self.picker.protocol_type()
    }

    /// Turns the artwork on or off, and says what the terminal is drawing it with so the
    /// answer to "why can I not see the posters" is one keypress away.
    pub fn toggle(&mut self) -> String {
        self.enabled = !self.enabled;
        if !self.enabled {
            return "Artwork off.".to_owned();
        }
        let protocol = match self.picker.protocol_type() {
            ProtocolType::Kitty => "kitty",
            ProtocolType::Sixel => "sixel",
            ProtocolType::Iterm2 => "iTerm2",
            ProtocolType::Halfblocks => "half-blocks - this terminal draws no graphics",
        };
        format!("Artwork on, drawn with {protocol}.")
    }

    /// The terminal's cell size in pixels, which is what turns "this panel is 30 columns
    /// wide" into "ask the CDN for something 300 pixels across".
    pub fn cell(&self) -> Size {
        let font = self.picker.font_size();
        Size::new(font.width, font.height)
    }

    /// Throws away everything already encoded for the terminal, keeping the pictures
    /// themselves.
    ///
    /// Kitty and iTerm2 hand the terminal the pixels once and place them again by id
    /// afterwards, so anything that owns the screen in the meantime can leave the ids
    /// pointing at nothing - mpv's own kitty output signs off by deleting every image it
    /// can see. Re-encoding costs a few milliseconds; a panel of blanks costs the
    /// screenshot.
    pub fn forget(&mut self) {
        self.encoded = Ring::new(ENCODED_CACHE);
    }

    /// Takes in whatever the fetchers have finished.
    pub fn drain(&mut self) {
        while let Ok((url, picture)) = self.arrived.try_recv() {
            self.asked.remove(&url);
            self.decoded.insert(url, picture);
        }
    }

    /// Draws `url` centred in `area`, asking for it if it has not arrived yet, and says
    /// whether anything was actually drawn.
    pub fn draw(&mut self, frame: &mut Frame, area: Rect, url: &str) -> bool {
        if !self.enabled || area.width == 0 || area.height == 0 {
            return false;
        }
        // Encoded for this panel already, in which case the picture behind it does not
        // even have to still be around.
        let key = (url.to_owned(), area.width, area.height);
        if self.encoded.get(&key).is_none() {
            let picture = match self.decoded.get(url) {
                Some(Some(picture)) => picture.clone(),
                // Fetched and found wanting. Asking a second time would only fail again,
                // once a frame, for as long as the cursor rests here.
                Some(None) => return false,
                None => {
                    self.request(url);
                    return false;
                }
            };
            let size = Size::new(area.width, area.height);
            // Triangle rather than the default nearest neighbour: a poster shrunk to a
            // twentieth of its width by picking one pixel in twenty is a field of noise.
            let protocol = self
                .picker
                .new_protocol(picture, size, Resize::Fit(Some(FilterType::Triangle)))
                .ok();
            self.encoded.insert(key.clone(), protocol);
        }

        let Some(Some(protocol)) = self.encoded.get(&key) else {
            return false;
        };
        frame.render_widget(Image::new(protocol), centre(area, protocol.size()));
        true
    }

    fn request(&mut self, url: &str) {
        if self.asked.contains(url) {
            return;
        }
        let mut waiting = self.wants.waiting.lock().expect("artwork queue poisoned");
        if waiting.fetchers == 0 {
            // Every fetcher is gone, so nothing will ever arrive. Mark the URL as
            // hopeless rather than queueing for a thread that is not there.
            drop(waiting);
            self.decoded.insert(url.to_owned(), None);
            return;
        }
        self.asked.insert(url.to_owned());
        // Whatever was pushed out is no longer being fetched, so it has to be free to be
        // asked for again the next time it is drawn.
        for stale in waiting.push(url.to_owned()) {
            self.asked.remove(&stale);
        }
        drop(waiting);
        self.wants.ready.notify_one();
    }
}

impl Drop for Gallery {
    fn drop(&mut self) {
        if let Ok(mut waiting) = self.wants.waiting.lock() {
            waiting.closed = true;
        }
        self.wants.ready.notify_all();
    }
}

/// The picture sits in the middle of the panel rather than in its top left corner: a
/// poster fitted to a tall column keeps its proportions and leaves a margin, and a margin
/// only on one side reads as a mistake.
fn centre(area: Rect, size: Size) -> Rect {
    Rect {
        x: area.x + area.width.saturating_sub(size.width) / 2,
        y: area.y + area.height.saturating_sub(size.height) / 2,
        width: size.width.min(area.width),
        height: size.height.min(area.height),
    }
}

/// Downloads and decodes one picture. The artwork CDN wants no credentials, so this needs
/// nothing from the API client.
fn fetch(client: &reqwest::blocking::Client, url: &str) -> Decoded {
    let body = client.get(url).send().ok()?.error_for_status().ok()?;
    let bytes = body.bytes().ok()?;
    // The extension in the URL is not to be trusted - Crunchyroll serves `.jpe` - so the
    // format is guessed from the bytes themselves.
    image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?
        .decode()
        .ok()
}

#[cfg(test)]
mod tests {
    use ratatui::layout::{Rect, Size};

    use super::{Ring, WAITING, Waiting, centre};

    #[test]
    fn a_ring_forgets_its_oldest_entry() {
        let mut ring: Ring<u8, &str> = Ring::new(2);
        ring.insert(1, "a");
        ring.insert(2, "b");
        ring.insert(3, "c");
        assert_eq!(ring.get(&1), None, "the oldest entry made way");
        assert_eq!(ring.get(&2), Some(&"b"));
        assert_eq!(ring.get(&3), Some(&"c"));

        // Overwriting is not a new entry, so it must not push anything else out.
        ring.insert(2, "B");
        assert_eq!(ring.get(&2), Some(&"B"));
        assert_eq!(ring.get(&3), Some(&"c"));
    }

    /// The picture asked for last is fetched first, and a long scroll lets go of the
    /// screens it passed rather than making the one it stopped on wait behind them.
    #[test]
    fn the_newest_picture_is_fetched_first_and_the_oldest_let_go() {
        let mut waiting = Waiting::default();
        let mut let_go = Vec::new();
        for index in 0..WAITING + 3 {
            let_go.extend(waiting.push(format!("poster {index}")));
        }
        assert_eq!(let_go, ["poster 0", "poster 1", "poster 2"]);
        assert_eq!(waiting.urls.len(), WAITING);
        assert_eq!(
            waiting.pop().as_deref(),
            Some(format!("poster {}", WAITING + 2).as_str())
        );
        assert_eq!(
            waiting.pop().as_deref(),
            Some(format!("poster {}", WAITING + 1).as_str())
        );
    }

    #[test]
    fn a_picture_sits_in_the_middle_of_its_panel() {
        let area = Rect::new(4, 2, 20, 10);
        assert_eq!(centre(area, Size::new(10, 6)), Rect::new(9, 4, 10, 6));
        // An odd margin leans left and up rather than overflowing the panel.
        assert_eq!(centre(area, Size::new(11, 5)), Rect::new(8, 4, 11, 5));
        // A picture larger than the panel is clamped instead of drawn outside it.
        assert_eq!(centre(area, Size::new(40, 40)), Rect::new(4, 2, 20, 10));
    }
}
