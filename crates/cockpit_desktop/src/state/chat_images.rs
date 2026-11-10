//! The images a message shows, from a Markdown image of a file. The daemon sends each one as it
//! sends a file to the file view (`agent.read_file`), and decides which files it reads. An agent
//! keeps its last [`KEPT`] images, so that the chat does not ask again for those it shows.

use std::path::Path;

use cockpit_protocol::{AgentId, Image, ReadFileParams, Request};

use super::app_state::{AgentEntry, AppState, Loadable};
use super::requests::{Outgoing, Purpose};
use crate::machine::MachineId;

/// An image is up to 8 MiB: a few dozen would hold hundreds of megabytes.
const KEPT: usize = 16;

/// The image at `path` in the agent's messages, once a message asked for it.
pub fn image<'state>(
    state: &'state AppState,
    agent: AgentId,
    path: &Path,
) -> Option<&'state Loadable<Image>> {
    let entry = state.agents.get(&agent)?;
    entry
        .images
        .iter()
        .find(|(kept, _)| kept == path)
        .map(|(_, image)| image)
}

/// A message shows the image: the next requests ask the daemon for it, once.
pub fn want(state: &mut AppState, agent: AgentId, path: &Path) {
    let Some(entry) = state.agents.get_mut(&agent) else {
        return;
    };
    if entry.images.iter().any(|(kept, _)| kept == path) {
        return;
    }
    if entry.images.len() >= KEPT {
        entry.images.pop_front();
    }
    entry
        .images
        .push_back((path.to_owned(), Loadable::NotRequested));
}

/// Asks for the agent's images that no request asked for yet.
pub(super) fn requests(entry: &mut AgentEntry, agent: AgentId, requests: &mut Vec<Outgoing>) {
    for (path, image) in &mut entry.images {
        if *image == Loadable::NotRequested {
            *image = Loadable::Loading;
            requests.push(Outgoing {
                request: Request::AgentReadFile(ReadFileParams {
                    agent,
                    path: path.clone(),
                }),
                purpose: Purpose::ChatImage {
                    agent,
                    path: path.clone(),
                },
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
        .and_then(|entry| entry.images.iter_mut().find(|(kept, _)| kept == path));
    if let Some((_, image)) = held {
        *image = match answer {
            Ok(image) => Loadable::Loaded(image),
            Err(message) => Loadable::Failed(message),
        };
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

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

    #[test]
    fn an_image_is_asked_for_once_and_then_shown() {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        let path = PathBuf::from("/tmp/shots/mockup.png");

        want(&mut state, agent(), &path);
        want(&mut state, agent(), &path);
        let mut asked = Vec::new();
        if let Some(entry) = state.agents.get_mut(&agent()) {
            requests(entry, agent(), &mut asked);
            requests(entry, agent(), &mut asked);
        }
        answered(&mut state, agent(), &path, Ok(png()));

        assert!(matches!(
            asked.as_slice(),
            [Outgoing {
                request: Request::AgentReadFile(ReadFileParams { path: asked, .. }),
                ..
            }] if *asked == path
        ));
        assert_eq!(
            image(&state, agent(), &path),
            Some(&Loadable::Loaded(png()))
        );
    }

    #[test]
    fn a_refused_image_keeps_the_daemons_sentence() {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        let path = PathBuf::from("/etc/secret.png");
        want(&mut state, agent(), &path);

        answered(&mut state, agent(), &path, Err("Not readable".to_owned()));

        assert_eq!(
            image(&state, agent(), &path),
            Some(&Loadable::Failed("Not readable".to_owned()))
        );
    }

    #[test]
    fn only_the_last_images_are_kept() {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        let path = |index: usize| PathBuf::from(format!("/tmp/shots/{index}.png"));

        for index in 0..=KEPT {
            want(&mut state, agent(), &path(index));
        }

        assert_eq!(image(&state, agent(), &path(0)), None);
        assert!(image(&state, agent(), &path(KEPT)).is_some());
    }
}
