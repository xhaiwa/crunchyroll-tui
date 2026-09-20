use std::collections::HashSet;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread;

use crate::api::CrunchyrollClient;
use crate::download::{DownloadOptions, Progress, download_episode, episode_info};
use crate::model::{CatalogItem, Playhead, Season, SeasonEpisode};

use super::SORTS;

/// Which list of series the catalogue pane is showing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Listing {
    /// An index into [`SORTS`].
    Browse(usize),
    Search(String),
    /// The account's own list, which is why it needs no parameter: there is one of it.
    Watchlist,
    /// What the account was last watching. It carries nothing for the same reason.
    History,
}

impl Listing {
    pub fn label(&self) -> String {
        match self {
            Self::Browse(sort) => SORTS[*sort].1.to_owned(),
            Self::Search(query) => format!("Search: {query}"),
            Self::Watchlist => "Watchlist".to_owned(),
            Self::History => "Continue watching".to_owned(),
        }
    }

    /// Every list the catalogue column cycles through, in order: the browse orders
    /// first, and the account's own lists after them.
    pub fn sources() -> Vec<Listing> {
        let mut sources: Vec<Listing> = (0..SORTS.len()).map(Listing::Browse).collect();
        sources.push(Listing::Watchlist);
        sources.push(Listing::History);
        sources
    }

    /// The list after this one. A search is not in the ring - it is left by going back
    /// rather than by cycling past it - so cycling from one starts the ring over.
    pub fn next(&self) -> Listing {
        let sources = Self::sources();
        let after = sources
            .iter()
            .position(|source| source == self)
            .map_or(0, |index| (index + 1) % sources.len());
        sources[after].clone()
    }
}

/// One episode on its way to disk: what to fetch, under what number, and the options to
/// fetch it with.
#[derive(Debug)]
pub struct Queued {
    /// Which download this is. The answers about it arrive long after the request went
    /// out and name it by this rather than by what it is, because the queue is a list
    /// the user can take rows out of and titles repeat where numbers do not.
    pub id: usize,
    pub episode: SeasonEpisode,
    /// Taken when the download was asked for rather than read when it starts. The
    /// interface goes on being used while the queue works through an hour of episodes,
    /// and the quality and the languages showing along the top by then are the next
    /// download's, not this one's.
    pub options: DownloadOptions,
}

/// Two downloads are the same request when they were queued under the same number,
/// which is the whole of what identifies one. Written out rather than derived because
/// the options behind it carry the callbacks a run reports through, and a box holding a
/// closure has nothing to compare.
impl PartialEq for Queued {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for Queued {}

#[derive(Debug, PartialEq, Eq)]
pub enum Request {
    Catalog(Listing),
    Seasons {
        series_id: String,
        audio: String,
        subs: String,
    },
    Episodes {
        season_id: String,
        audio: String,
        subs: String,
    },
    /// Put the series on the watchlist, or take it off if it is already there. Which of
    /// the two it is takes a request of its own to find out, and that answer is only
    /// worth having on the thread that is about to act on it: asking from the interface
    /// would mean the interface waiting on the network, which is the one thing this
    /// channel exists to prevent.
    Watchlist {
        series_id: String,
        series_title: String,
    },
    /// Put the episode's playhead at `seconds`, which is how Crunchyroll is told the
    /// episode has been watched or has not been. `label` is what the notice calls it,
    /// carried along because by the time the answer comes back the cursor may be
    /// somewhere else entirely.
    Playhead {
        episode_id: String,
        label: String,
        seconds: u32,
    },
    /// How far the account has got into each of a season's episodes. The season travels
    /// with the ids so that the answer can be checked against the column that is open by
    /// the time it arrives, the way every other answer here is.
    Playheads {
        season_id: String,
        episode_ids: Vec<String>,
    },
    /// One episode to write to disk. It leaves through the same `send` as everything
    /// else and lands on a thread of its own: see [`Worker`]. Boxed because it is much
    /// the largest thing this enum can hold - an episode and a whole set of options -
    /// and every other request would otherwise be that size too.
    Download(Box<Queued>),
}

/// Answers carry back what was asked for, so an answer to a question the user has
/// already moved on from can be recognised and dropped.
pub enum Response {
    Catalog {
        listing: Listing,
        result: Result<Vec<CatalogItem>, String>,
    },
    Seasons {
        series_id: String,
        result: Result<Vec<Season>, String>,
    },
    Episodes {
        season_id: String,
        result: Result<Vec<SeasonEpisode>, String>,
    },
    Playheads {
        season_id: String,
        result: Result<Vec<Playhead>, String>,
    },
    /// What became of a change to the account, as the sentence to put on the status
    /// line. Nothing on screen shows the watchlist or the history, so that sentence is
    /// the whole of what the user is told about it and it has to say what happened to
    /// what - which is why the answer carries the words rather than the ids.
    Account { result: Result<String, String> },
    /// How a queued download is getting on. `id` is the number the request was given,
    /// because the row it belongs to may have moved or gone by the time this arrives -
    /// an answer that named the episode would find the wrong one in a queue holding the
    /// same episode twice.
    Download { id: usize, update: Update },
}

/// What becomes of one queued episode, in the order it happens to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Update {
    /// It has reached the front of the queue and the thread has started on it.
    Started,
    /// Which part of the episode is moving and how far it has got, as
    /// [`Progress::Stage`] gives it.
    Stage {
        stage: String,
        done: u64,
        total: u64,
    },
    /// It is over, one way or the other. A failure carries the reason, because that is
    /// all the user will ever see of it: the download thread has no terminal to print
    /// a backtrace to.
    Finished(Result<(), String>),
}

/// What the status line says once the watchlist has changed.
fn watchlist_notice(series_title: &str, added: bool) -> String {
    if added {
        format!("Added {series_title} to the watchlist")
    } else {
        format!("Removed {series_title} from the watchlist")
    }
}

/// And what it says once a playhead has moved. It reads the playhead rather than the key
/// that was pressed, because the playhead is what the account now holds - and the two
/// cannot disagree, since an episode with no running time to aim at is turned away before
/// it ever gets here.
fn playhead_notice(label: &str, seconds: u32) -> String {
    if seconds == 0 {
        format!("Marked {label} unwatched")
    } else {
        format!("Marked {label} watched")
    }
}

/// How many series one catalogue page holds. Crunchyroll caps `n` at 100.
const CATALOG_PAGE: usize = 100;

/// The queue: episodes written to disk one after another, on a thread that nothing else
/// waits on.
///
/// Each one runs with the options it was queued with, with two things settled here. It
/// writes a file rather than playing, whatever the run was started as - the queue is
/// what `d` means and mpv is what `p` means. And it is given a reporter, which is what
/// makes a download something that can happen while the interface is on screen: with
/// one set, nothing in `download.rs` prints a line or draws a bar, and every step of it
/// arrives here as a [`Progress`] to be passed on as an answer like any other.
///
/// A download that fails takes the episode with it and nothing else. The queue is a
/// list of things the user asked for separately, and an episode Crunchyroll will not
/// hand over is no reason to drop the nine behind it.
fn spawn_downloads(
    client: CrunchyrollClient,
    outbox: Sender<Response>,
    abandoned: Arc<Mutex<HashSet<usize>>>,
) -> Sender<Queued> {
    let (downloads, queue) = channel::<Queued>();
    thread::spawn(move || {
        for job in queue {
            let Queued {
                id,
                episode,
                options,
            } = job;
            // Dropped from the panel while it waited here. Nothing was started, so
            // nothing is said about it: the row it would have answered to is gone.
            if abandoned
                .lock()
                .expect("abandoned downloads poisoned")
                .remove(&id)
            {
                continue;
            }
            if outbox
                .send(Response::Download {
                    id,
                    update: Update::Started,
                })
                .is_err()
            {
                break;
            }
            let reporter = Sender::clone(&outbox);
            let options = DownloadOptions {
                play: false,
                reporter: Some(Arc::new(move |progress| {
                    // The sentences are written for a terminal being scrolled past -
                    // every locale asked for, every one that was not there - and the
                    // queue draws one row per episode, which the stage is a better use
                    // of. So the words are let go of here and the numbers go on.
                    if let Progress::Stage { stage, done, total } = progress {
                        let _ = reporter.send(Response::Download {
                            id,
                            update: Update::Stage { stage, done, total },
                        });
                    }
                })),
                ..options
            };
            let info = episode_info(&episode);
            let result = download_episode(&client, &episode.id, &info, &options)
                .map_err(|error| format!("{error:#}"));
            if outbox
                .send(Response::Download {
                    id,
                    update: Update::Finished(result),
                })
                .is_err()
            {
                break;
            }
        }
    });
    downloads
}

/// The API client is blocking and the interface has to stay responsive, so every
/// request runs off the interface's thread and comes back through a channel.
///
/// Two threads rather than one, because the work is two shapes. Browsing is a string of
/// short questions - a catalogue page, a season, a set of playheads - which one thread
/// answers one after another with nothing lost by the ordering. A download is an hour,
/// and queueing one behind the other kind would be an hour in which no column could be
/// filled and no language changed. So downloads get a queue of their own, in the order
/// they were asked for and one episode at a time: a single download is already as
/// parallel inside as the connection can take, and running two of them at once only
/// splits the same pipe in half.
///
/// Both threads answer down the same channel, since the interface reads it in one place
/// and every answer says what it was for.
pub struct Worker {
    requests: Sender<Request>,
    downloads: Sender<Queued>,
    /// The numbers of downloads dropped from the panel before they began. A request
    /// already on the queue's channel cannot be taken off it again, and a message sent
    /// after it would arrive behind the download it was meant to stop - so what has
    /// been dropped is written down where the thread looks before it starts the next
    /// one.
    abandoned: Arc<Mutex<HashSet<usize>>>,
    responses: Receiver<Response>,
    /// The far end of a detached worker's channel, held open so a test can read back
    /// what the interface asked for. A command that only sends leaves no other trace,
    /// and which item it picked is the whole of what there is to check.
    #[cfg(test)]
    inbox: Option<Receiver<Request>>,
    /// And the same for the queue, which is the other place a command ends up.
    #[cfg(test)]
    queue: Option<Receiver<Queued>>,
}

impl Worker {
    pub fn spawn(client: CrunchyrollClient) -> Self {
        let (requests, inbox) = channel::<Request>();
        let (outbox, responses) = channel::<Response>();
        let abandoned = Arc::new(Mutex::new(HashSet::new()));
        let downloads = spawn_downloads(client.clone(), outbox.clone(), Arc::clone(&abandoned));
        // The thread ends when the app drops its end of the channel, and a request that
        // is still in flight then finishes into a closed channel rather than blocking
        // the quit.
        thread::spawn(move || {
            for request in inbox {
                let response = match request {
                    Request::Catalog(listing) => {
                        let result = match &listing {
                            Listing::Browse(sort) => client.browse(SORTS[*sort].0, CATALOG_PAGE, 0),
                            Listing::Search(query) => client.search(query, CATALOG_PAGE),
                            Listing::Watchlist => client.watchlist(CATALOG_PAGE),
                            Listing::History => client.history(CATALOG_PAGE),
                        };
                        Response::Catalog {
                            listing,
                            result: result.map_err(|error| format!("{error:#}")),
                        }
                    }
                    Request::Seasons {
                        series_id,
                        audio,
                        subs,
                    } => {
                        let result = client.seasons(&series_id, &audio, &subs);
                        Response::Seasons {
                            series_id,
                            result: result.map_err(|error| format!("{error:#}")),
                        }
                    }
                    Request::Episodes {
                        season_id,
                        audio,
                        subs,
                    } => {
                        let result = client.season_episodes(&season_id, &audio, &subs);
                        Response::Episodes {
                            season_id,
                            result: result.map_err(|error| format!("{error:#}")),
                        }
                    }
                    Request::Watchlist {
                        series_id,
                        series_title,
                    } => {
                        let result = client.in_watchlist(&series_id).and_then(|listed| {
                            if listed {
                                client.watchlist_remove(&series_id)
                            } else {
                                client.watchlist_add(&series_id)
                            }
                            .map(|()| watchlist_notice(&series_title, !listed))
                        });
                        Response::Account {
                            result: result.map_err(|error| format!("{error:#}")),
                        }
                    }
                    Request::Playhead {
                        episode_id,
                        label,
                        seconds,
                    } => {
                        let result = client
                            .set_playhead(&episode_id, seconds)
                            .map(|()| playhead_notice(&label, seconds));
                        Response::Account {
                            result: result.map_err(|error| format!("{error:#}")),
                        }
                    }
                    Request::Playheads {
                        season_id,
                        episode_ids,
                    } => {
                        let result = client.playheads(&episode_ids);
                        Response::Playheads {
                            season_id,
                            result: result.map_err(|error| format!("{error:#}")),
                        }
                    }
                    // Sent, never received: `send` puts these on the download thread's
                    // channel instead of this one, which is the whole point of there
                    // being two of them.
                    Request::Download(_) => continue,
                };
                if outbox.send(response).is_err() {
                    break;
                }
            }
        });
        Self {
            requests,
            downloads,
            abandoned,
            responses,
            #[cfg(test)]
            inbox: None,
            #[cfg(test)]
            queue: None,
        }
    }

    /// A worker with nothing behind it: no answer ever comes, which is all a test of the
    /// interface itself needs. What it is sent is kept rather than dropped, so a test
    /// can also ask what the interface would have gone to the network for.
    #[cfg(test)]
    pub fn detached() -> Self {
        let (requests, inbox) = channel::<Request>();
        let (downloads, queue) = channel::<Queued>();
        let (_, responses) = channel::<Response>();
        Self {
            requests,
            downloads,
            abandoned: Arc::new(Mutex::new(HashSet::new())),
            responses,
            inbox: Some(inbox),
            queue: Some(queue),
        }
    }

    /// Everything sent since this was last asked, the downloads after the rest. The two
    /// go down channels of their own, so nothing here can say which came first - and no
    /// command sends both.
    #[cfg(test)]
    pub fn sent(&self) -> Vec<Request> {
        let mut sent: Vec<Request> = match &self.inbox {
            Some(inbox) => inbox.try_iter().collect(),
            None => Vec::new(),
        };
        if let Some(queue) = &self.queue {
            sent.extend(queue.try_iter().map(|job| Request::Download(Box::new(job))));
        }
        sent
    }

    pub fn send(&self, request: Request) {
        match request {
            Request::Download(job) => {
                let _ = self.downloads.send(*job);
            }
            request => {
                let _ = self.requests.send(request);
            }
        }
    }

    /// Forgets a download that has not started yet. See `abandoned` above for why it is
    /// written down rather than sent.
    pub fn abandon(&self, id: usize) {
        self.abandoned
            .lock()
            .expect("abandoned downloads poisoned")
            .insert(id);
    }

    pub fn try_recv(&self) -> Option<Response> {
        self.responses.try_recv().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::{Listing, SORTS, playhead_notice, watchlist_notice};

    /// What the order key walks through. The browse orders come first and the account's
    /// own lists after them, and the ring closes: the last list leads back to the first
    /// rather than to nowhere, so holding the key down can only go round.
    #[test]
    fn the_lists_cycle_in_a_closed_ring() {
        let sources = Listing::sources();
        assert_eq!(sources.len(), SORTS.len() + 2);
        for (index, source) in sources.iter().take(SORTS.len()).enumerate() {
            assert_eq!(*source, Listing::Browse(index));
        }
        assert_eq!(sources[SORTS.len()], Listing::Watchlist);
        assert_eq!(sources.last(), Some(&Listing::History));
        for pair in sources.windows(2) {
            assert_eq!(pair[0].next(), pair[1], "{:?} leads somewhere odd", pair[0]);
        }
        assert_eq!(
            Listing::History.next(),
            Listing::Browse(0),
            "the last list has to lead back to the first"
        );
    }

    /// A search is a detour rather than a stop on the ring: it is left by going back.
    /// Cycling out of one still has to land somewhere, and the start of the ring is the
    /// only answer that does not depend on where the search was begun.
    #[test]
    fn cycling_out_of_a_search_starts_the_ring_over() {
        assert_eq!(
            Listing::Search("frieren".to_owned()).next(),
            Listing::Browse(0)
        );
    }

    /// The label is both what the header prints and what a click on the header is aimed
    /// at, so every list needs one - and the search has to say back what was typed into
    /// it, since that is the only place the query is shown.
    #[test]
    fn every_list_says_which_one_it_is() {
        assert_eq!(Listing::Browse(0).label(), "Popular");
        assert_eq!(Listing::Browse(1).label(), "Recently added");
        assert_eq!(Listing::Browse(2).label(), "A to Z");
        assert_eq!(Listing::Watchlist.label(), "Watchlist");
        assert_eq!(Listing::History.label(), "Continue watching");
        assert_eq!(
            Listing::Search("frieren".to_owned()).label(),
            "Search: frieren"
        );
    }

    /// Nothing on screen shows the watchlist or the history, so these sentences are the
    /// whole of what the user is told about a change to either. A sentence that only
    /// said something had been done would leave them wondering which way it went, and
    /// which of the two keys they actually hit.
    #[test]
    fn every_notice_says_what_happened_to_what() {
        assert_eq!(
            watchlist_notice("Frieren", true),
            "Added Frieren to the watchlist"
        );
        assert_eq!(
            watchlist_notice("Frieren", false),
            "Removed Frieren from the watchlist"
        );
        assert_eq!(playhead_notice("E4", 1461), "Marked E4 watched");
        assert_eq!(playhead_notice("E4", 0), "Marked E4 unwatched");
    }
}
