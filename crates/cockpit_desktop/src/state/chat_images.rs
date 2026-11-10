//! The images a message shows, from a Markdown image of a file. The daemon sends each one as it
//! sends a file to the file view (`agent.read_file`), and decides which files it reads. An agent
//! keeps the images on screen, and its last [`KEPT`] others, so that the chat does not ask again
//! for those it shows.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use cockpit_protocol::{AgentId, Image, ReadFileParams, Request};

use super::app_state::{AgentEntry, AppState, Loadable};
use super::requests::{Outgoing, Purpose};
use crate::machine::MachineId;

/// An image is up to 8 MiB: a few dozen would hold hundreds of megabytes.
const KEPT: usize = 16;

/// An image the agent's messages show. Shared, so that a view can hold it without a copy.
#[derive(Debug, Clone, PartialEq)]
pub struct KeptImage {
    path: PathBuf,
    image: Loadable<Arc<Image>>,
    /// The places on screen that show it.
    shown: usize,
}

/// The image at `path` in the agent's messages, once a message asked for it.
pub fn image(state: &AppState, agent: AgentId, path: &Path) -> Option<Loadable<Arc<Image>>> {
    let entry = state.agents.get(&agent)?;
    entry
        .images
        .iter()
        .find(|kept| kept.path == path)
        .map(|kept| kept.image.clone())
}

/// A place on screen shows the image: the next requests ask the daemon for it, once.
pub fn show(state: &mut AppState, agent: AgentId, path: &Path) {
    let Some(entry) = state.agents.get_mut(&agent) else {
        return;
    };
    let index = entry.images.iter().position(|kept| kept.path == path);
    let mut kept = index
        .and_then(|index| entry.images.remove(index))
        .unwrap_or_else(|| KeptImage {
            path: path.to_owned(),
            image: Loadable::NotRequested,
            shown: 0,
        });
    kept.shown = kept.shown.saturating_add(1);
    entry.images.push_back(kept);
    drop_hidden(entry);
}

/// A place on screen no longer shows the image. One that failed goes, so that showing it again
/// asks again.
pub fn hide(state: &mut AppState, agent: AgentId, path: &Path) {
    let Some(entry) = state.agents.get_mut(&agent) else {
        return;
    };
    let Some(index) = entry.images.iter().position(|kept| kept.path == path) else {
        return;
    };
    let gone = entry.images.get_mut(index).is_some_and(|kept| {
        kept.shown = kept.shown.saturating_sub(1);
        kept.shown == 0 && matches!(kept.image, Loadable::Failed(_))
    });
    if gone {
        entry.images.remove(index);
    }
    drop_hidden(entry);
}

/// Drops the oldest images no place shows, beyond [`KEPT`].
fn drop_hidden(entry: &mut AgentEntry) {
    let mut hidden = entry.images.iter().filter(|kept| kept.shown == 0).count();
    entry.images.retain(|kept| {
        let drops = kept.shown == 0 && hidden > KEPT;
        if drops {
            hidden -= 1;
        }
        !drops
    });
}

/// Asks for the agent's images that no request asked for yet.
pub(super) fn requests(entry: &mut AgentEntry, agent: AgentId, requests: &mut Vec<Outgoing>) {
    for kept in &mut entry.images {
        if kept.image == Loadable::NotRequested {
            kept.image = Loadable::Loading;
            let path = kept.path.clone();
            requests.push(Outgoing {
                request: Request::AgentReadFile(ReadFileParams {
                    agent,
                    path: path.clone(),
                }),
                purpose: Purpose::ChatImage { agent, path },
                to: MachineId::THIS,
            });
        }
    }
}

/// The daemon's answer for the image: the image, or the sentence that says why it cannot show.
pub(super) fn answered(
    state: &mut AppState,
    agent: AgentId,
    path: &Path,
    answer: Result<Image, String>,
) {
    let held = state
        .agents
        .get_mut(&agent)
        .and_then(|entry| entry.images.iter_mut().find(|kept| kept.path == path));
    if let Some(kept) = held {
        kept.image = match answer {
            Ok(image) => Loadable::Loaded(Arc::new(image)),
            Err(message) => Loadable::Failed(message),
        };
    }
}

#[cfg(test)]
mod tests {
    use cockpit_protocol::{AgentStatus, ImageMediaType};

    use super::*;
    use crate::state::app_state::tests::{state_with, summary};

    fn agent() -> AgentId {
        AgentId::new(1)
    }

    fn png() -> Image {
        Image {
            media_type: ImageMediaType::Png,
            data: "iVBORw0KGgo=".to_owned(),
        }
    }

    fn asked(state: &mut AppState) -> Vec<Outgoing> {
        let mut asked = Vec::new();
        if let Some(entry) = state.agents.get_mut(&agent()) {
            requests(entry, agent(), &mut asked);
        }
        asked
    }

    #[test]
    fn an_image_is_asked_for_once_and_then_shown() {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        let path = PathBuf::from("/tmp/shots/mockup.png");

        show(&mut state, agent(), &path);
        show(&mut state, agent(), &path);
        let first = asked(&mut state);
        let second = asked(&mut state);
        answered(&mut state, agent(), &path, Ok(png()));

        assert!(matches!(
            first.as_slice(),
            [Outgoing {
                request: Request::AgentReadFile(ReadFileParams { path: asked, .. }),
                ..
            }] if *asked == path
        ));
        assert!(second.is_empty());
        assert_eq!(
            image(&state, agent(), &path),
            Some(Loadable::Loaded(Arc::new(png())))
        );
    }

    #[test]
    fn a_refused_image_keeps_the_daemons_sentence_and_is_asked_again_once_hidden() {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        let path = PathBuf::from("/etc/secret.png");
        show(&mut state, agent(), &path);
        asked(&mut state);

        answered(&mut state, agent(), &path, Err("Not readable".to_owned()));
        let refused = image(&state, agent(), &path);
        hide(&mut state, agent(), &path);
        show(&mut state, agent(), &path);

        assert_eq!(refused, Some(Loadable::Failed("Not readable".to_owned())));
        assert_eq!(asked(&mut state).len(), 1);
    }

    /// A chat that shows more images than are kept still shows each of them.
    #[test]
    fn the_images_on_screen_stay_and_only_the_last_others_are_kept() {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        let path = |index: usize| PathBuf::from(format!("/tmp/shots/{index}.png"));

        for index in 0..=KEPT {
            show(&mut state, agent(), &path(index));
        }
        let all_shown = (0..=KEPT).all(|index| image(&state, agent(), &path(index)).is_some());
        for index in 0..=KEPT {
            hide(&mut state, agent(), &path(index));
        }

        assert!(all_shown);
        assert_eq!(image(&state, agent(), &path(0)), None);
        assert!(image(&state, agent(), &path(1)).is_some());
        assert!(image(&state, agent(), &path(KEPT)).is_some());
    }
}
