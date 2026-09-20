use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;

use crate::api::CrunchyrollClient;
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

/// The API client is blocking and the interface has to stay responsive, so every
/// request runs on this thread and comes back through a channel.
pub struct Worker {
    requests: Sender<Request>,
    responses: Receiver<Response>,
    /// The far end of a detached worker's channel, held open so a test can read back
    /// what the interface asked for. A command that only sends leaves no other trace,
    /// and which item it picked is the whole of what there is to check.
    #[cfg(test)]
    inbox: Option<Receiver<Request>>,
}

impl Worker {
    pub fn spawn(client: CrunchyrollClient) -> Self {
        let (requests, inbox) = channel::<Request>();
        let (outbox, responses) = channel::<Response>();
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
                };
                if outbox.send(response).is_err() {
                    break;
                }
            }
        });
        Self {
            requests,
            responses,
            #[cfg(test)]
            inbox: None,
        }
    }

    /// A worker with nothing behind it: no answer ever comes, which is all a test of the
    /// interface itself needs. What it is sent is kept rather than dropped, so a test
    /// can also ask what the interface would have gone to the network for.
    #[cfg(test)]
    pub fn detached() -> Self {
        let (requests, inbox) = channel::<Request>();
        let (_, responses) = channel::<Response>();
        Self {
            requests,
            responses,
            inbox: Some(inbox),
        }
    }

    /// Everything sent since this was last asked.
    #[cfg(test)]
    pub fn sent(&self) -> Vec<Request> {
        match &self.inbox {
            Some(inbox) => inbox.try_iter().collect(),
            None => Vec::new(),
        }
    }

    pub fn send(&self, request: Request) {
        let _ = self.requests.send(request);
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
