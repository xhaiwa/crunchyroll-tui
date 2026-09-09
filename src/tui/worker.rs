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
}

impl Listing {
    pub fn label(&self) -> String {
        match self {
            Self::Browse(sort) => SORTS[*sort].1.to_owned(),
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
                            Listing::Browse(sort) => {
                                client.browse(SORTS[*sort].0, CATALOG_PAGE, 0)
                            }
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
