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
    /// What the account has put on its own list.
    Watchlist,
    Search(String),
}

impl Listing {
    /// How many listings the order key steps through: the catalogue in each order
    /// Crunchyroll offers, and then the account's own list. A search is not among them,
    /// since it is arrived at by typing rather than by stepping.
    pub const SOURCES: usize = SORTS.len() + 1;

    /// The `index`th of those, the ones past [`SORTS`] being the account's list.
    pub fn nth_source(index: usize) -> Self {
        if index < SORTS.len() {
            Self::Browse(index)
        } else {
            Self::Watchlist
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::Browse(sort) => SORTS[*sort].1.to_owned(),
            Self::Watchlist => "My list".to_owned(),
            Self::Search(query) => format!("Search: {query}"),
        }
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
                            Listing::Watchlist => client.watchlist(CATALOG_PAGE),
                            Listing::Search(query) => client.search(query, CATALOG_PAGE),
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

    /// The order key walks every catalogue order and then the account's own list,
    /// rather than stepping off the end of [`SORTS`] into a panic.
    #[test]
    fn the_sources_are_the_orders_and_the_watchlist() {
        let sources: Vec<Listing> = (0..Listing::SOURCES).map(Listing::nth_source).collect();
        for (index, order) in SORTS.iter().enumerate() {
            assert_eq!(sources[index], Listing::Browse(index));
            assert_eq!(sources[index].label(), order.1);
        }
        assert_eq!(sources.last(), Some(&Listing::Watchlist));
        assert_eq!(Listing::Watchlist.label(), "My list");
    }
}
