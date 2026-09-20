use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;

use crate::api::CrunchyrollClient;
use crate::model::{CatalogItem, Season, SeasonEpisode};

use super::SORTS;

/// Which list of series the catalogue pane is showing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Listing {
    /// An index into [`SORTS`].
    Browse(usize),
    Search(String),
    /// The account's own list, which is why it needs no parameter: there is one of it.
    Watchlist,
}

impl Listing {
    pub fn label(&self) -> String {
        match self {
            Self::Browse(sort) => SORTS[*sort].1.to_owned(),
            Self::Search(query) => format!("Search: {query}"),
            Self::Watchlist => "Watchlist".to_owned(),
        }
    }

    /// Every list the catalogue column cycles through, in order: the browse orders
    /// first, and the account's own lists after them.
    pub fn sources() -> Vec<Listing> {
        let mut sources: Vec<Listing> = (0..SORTS.len()).map(Listing::Browse).collect();
        sources.push(Listing::Watchlist);
        sources
    }

    /// The list after this one. A search is not in the ring - it is left by going back
    /// rather than by cycling past it - so cycling from one starts the ring over.
    pub fn next(&self) -> Listing {
        let sources = Self::sources();
        let next = sources
            .iter()
            .position(|source| source == self)
            .map_or(0, |index| (index + 1) % sources.len());
        sources[next].clone()
    }
}

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
}

/// How many series one catalogue page holds. Crunchyroll caps `n` at 100.
const CATALOG_PAGE: usize = 100;

/// The API client is blocking and the interface has to stay responsive, so every
/// request runs on this thread and comes back through a channel.
pub struct Worker {
    requests: Sender<Request>,
    responses: Receiver<Response>,
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
                };
                if outbox.send(response).is_err() {
                    break;
                }
            }
        });
        Self {
            requests,
            responses,
        }
    }

    /// A worker with nothing behind it: requests go nowhere and no answer ever comes,
    /// which is all a test of the interface itself needs.
    #[cfg(test)]
    pub fn detached() -> Self {
        let (requests, _) = channel::<Request>();
        let (_, responses) = channel::<Response>();
        Self {
            requests,
            responses,
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
    use super::{Listing, SORTS};

    /// What `o` walks through, and the one thing that makes holding it down safe: the
    /// ring closes. Every browse order comes first and the account's own lists after
    /// them, and the last of them leads back to the first rather than to nowhere.
    #[test]
    fn the_catalogue_lists_cycle_in_a_ring() {
        let sources = Listing::sources();
        assert_eq!(sources.len(), SORTS.len() + 1);
        assert_eq!(sources[0], Listing::Browse(0));
        assert_eq!(sources[SORTS.len() - 1], Listing::Browse(SORTS.len() - 1));
        assert_eq!(sources.last(), Some(&Listing::Watchlist));
        for pair in sources.windows(2) {
            assert_eq!(
                pair[0].next(),
                pair[1],
                "{:?} leads nowhere useful",
                pair[0]
            );
        }
        assert_eq!(
            Listing::Watchlist.next(),
            Listing::Browse(0),
            "the last list has to lead back to the first"
        );
    }

    /// A search is a detour rather than a stop on the ring, so there is no list after it
    /// to find. Cycling out of one has to land somewhere definite all the same, and the
    /// start of the ring is the only answer that does not depend on where the search was
    /// begun.
    #[test]
    fn cycling_out_of_a_search_starts_the_ring_over() {
        assert_eq!(
            Listing::Search("frieren".to_owned()).next(),
            Listing::Browse(0)
        );
    }

    /// The label is what the header draws and what a click on the header is aimed at, so
    /// every list needs one, and a search has to show back what was typed into it.
    #[test]
    fn every_list_says_what_it_is() {
        assert_eq!(Listing::Browse(0).label(), "Popular");
        assert_eq!(Listing::Browse(1).label(), "Recently added");
        assert_eq!(Listing::Browse(2).label(), "A to Z");
        assert_eq!(Listing::Watchlist.label(), "Watchlist");
        assert_eq!(
            Listing::Search("frieren".to_owned()).label(),
            "Search: frieren"
        );
    }
}
