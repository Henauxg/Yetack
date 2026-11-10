//! The keyboard shortcuts that run a command, which the Settings dialog can change, and the ones
//! whose meaning comes from where they are pressed, which it only lists.
//!
//! A chord holds a key as the page's `KeyboardEvent.key` names it: one character in lower case,
//! or a key's name such as `F2` or `ArrowUp`. The page script and the app both read keys that way.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use strum::{EnumString, IntoStaticStr, VariantArray};

/// How the modifiers and the key of a chord are written, as in "Ctrl+Shift+H".
const CTRL: &str = "Ctrl";
const ALT: &str = "Alt";
const SHIFT: &str = "Shift";
const JOIN: char = '+';

/// Keys that keep their meaning whatever the modifiers.
const ESCAPE: &str = "Escape";
const TAB: &str = "Tab";

/// The key names a chord shows with a symbol.
const ARROWS: [(&str, &str); 4] = [
    ("ArrowUp", "↑"),
    ("ArrowDown", "↓"),
    ("ArrowLeft", "←"),
    ("ArrowRight", "→"),
];
const SPACE: &str = " ";
const SPACE_NAME: &str = "Space";

/// The keys pressed alone that are no chord: a chord waits for the key that goes with them.
const MODIFIER_KEYS: [&str; 6] = ["Control", "Shift", "Alt", "Meta", "AltGraph", "OS"];

/// A command a shortcut runs. The serde names are the preferences file's.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    EnumString,
    IntoStaticStr,
    VariantArray,
)]
#[strum(serialize_all = "snake_case")]
pub enum Command {
    Switcher,
    NewAgent,
    OpenFile,
    FilesPane,
    AgentMap,
    Rename,
    MarkToHandle,
    CloseAgent,
    MoveUp,
    MoveDown,
    ReloadFile,
}

/// Where the page script takes a command's shortcut.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Reach {
    /// From anywhere, a text field or a dialog included.
    Everywhere,
    /// Outside text fields and dialogs, and from the title fields.
    PageOrTitle,
    /// Outside dialogs and text fields other than the message.
    OutsideFields,
}

/// The groups the Settings dialog lists shortcuts in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, VariantArray)]
pub enum Group {
    Anywhere,
    SelectedAgent,
    Message,
    Answers,
    FileView,
}

impl Group {
    pub fn title(self) -> &'static str {
        match self {
            Self::Anywhere => "Anywhere",
            Self::SelectedAgent => "The selected agent",
            Self::Message => "Message",
            Self::Answers => "Answers",
            Self::FileView => "File view",
        }
    }
}

impl Command {
    pub fn label(self) -> &'static str {
        match self {
            Self::Switcher => "Switch to another agent",
            Self::NewAgent => "Start a new agent",
            Self::OpenFile => "Open a file",
            Self::FilesPane => "Show or hide the files pane",
            Self::AgentMap => "Open the agent map",
            Self::Rename => "Rename",
            Self::MarkToHandle => "Mark to handle",
            Self::CloseAgent => "Close",
            Self::MoveUp => "Move up in the sidebar",
            Self::MoveDown => "Move down in the sidebar",
            Self::ReloadFile => "Read the file again",
        }
    }

    pub fn group(self) -> Group {
        match self {
            Self::Switcher | Self::NewAgent | Self::OpenFile | Self::FilesPane | Self::AgentMap => {
                Group::Anywhere
            }
            Self::Rename
            | Self::MarkToHandle
            | Self::CloseAgent
            | Self::MoveUp
            | Self::MoveDown => Group::SelectedAgent,
            Self::ReloadFile => Group::FileView,
        }
    }

    pub fn reach(self) -> Reach {
        match self {
            Self::Rename => Reach::PageOrTitle,
            Self::MoveUp | Self::MoveDown => Reach::OutsideFields,
            Self::Switcher
            | Self::NewAgent
            | Self::OpenFile
            | Self::FilesPane
            | Self::AgentMap
            | Self::MarkToHandle
            | Self::CloseAgent
            | Self::ReloadFile => Reach::Everywhere,
        }
    }

    pub fn default_chord(self) -> Chord {
        match self {
            Self::Switcher => Chord::ctrl("k"),
            Self::NewAgent => Chord::ctrl("n"),
            Self::OpenFile => Chord::ctrl("o"),
            Self::FilesPane => Chord::ctrl("b"),
            Self::AgentMap => Chord::ctrl("m"),
            Self::Rename => Chord::plain("F2"),
            Self::MarkToHandle => Chord::ctrl_shift("h"),
            Self::CloseAgent => Chord::ctrl_shift("w"),
            Self::MoveUp => Chord::alt("ArrowUp"),
            Self::MoveDown => Chord::alt("ArrowDown"),
            Self::ReloadFile => Chord::plain("F5"),
        }
    }
}

/// A shortcut the Settings dialog lists and no setting changes: its meaning comes from where it is
/// pressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FixedShortcut {
    pub label: &'static str,
    /// Where it acts, when the label alone does not say.
    pub context: Option<&'static str>,
    /// The keys, as they show: each inner list is one chord, the outer one the alternatives.
    pub keys: &'static [&'static [&'static str]],
    pub group: Group,
}

pub const FIXED: [FixedShortcut; 9] = [
    FixedShortcut {
        label: "Close what is open",
        context: None,
        keys: &[&["Esc"]],
        group: Group::Anywhere,
    },
    FixedShortcut {
        label: "Send",
        context: None,
        keys: &[&["Enter"]],
        group: Group::Message,
    },
    FixedShortcut {
        label: "New line",
        context: None,
        keys: &[&[SHIFT, "Enter"]],
        group: Group::Message,
    },
    FixedShortcut {
        label: "Send when the turn ends",
        context: None,
        keys: &[&[ALT, "Enter"]],
        group: Group::Message,
    },
    FixedShortcut {
        label: "Commands and skills",
        context: None,
        keys: &[&["/"]],
        group: Group::Message,
    },
    FixedShortcut {
        label: "Reply to the selected text",
        context: Some("in the chat"),
        keys: &[&[CTRL, "R"]],
        group: Group::Message,
    },
    FixedShortcut {
        label: "Allow or deny",
        context: Some("an approval or a plan"),
        keys: &[&["Y"], &["N"]],
        group: Group::Answers,
    },
    FixedShortcut {
        label: "Pick an answer",
        context: Some("a question"),
        keys: &[&["1–9"]],
        group: Group::Answers,
    },
    FixedShortcut {
        label: "Close the file",
        context: None,
        keys: &[&[CTRL, "W"]],
        group: Group::FileView,
    },
];

/// A key with the modifiers held with it. Ctrl stands for Cmd on macOS too.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Chord {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    /// One character in lower case, or a key's name.
    pub key: String,
}

impl Chord {
    /// The chord of a key press, as the page reports it. `None` for a modifier pressed alone.
    pub fn of(key: &str, ctrl: bool, alt: bool, shift: bool) -> Option<Self> {
        if key.is_empty() || MODIFIER_KEYS.contains(&key) {
            return None;
        }
        Some(Self {
            ctrl,
            alt,
            shift,
            key: normalized(key),
        })
    }

    fn plain(key: &str) -> Self {
        Self {
            ctrl: false,
            alt: false,
            shift: false,
            key: key.to_owned(),
        }
    }

    fn ctrl(key: &str) -> Self {
        Self {
            ctrl: true,
            ..Self::plain(key)
        }
    }

    fn ctrl_shift(key: &str) -> Self {
        Self {
            shift: true,
            ..Self::ctrl(key)
        }
    }

    fn alt(key: &str) -> Self {
        Self {
            alt: true,
            ..Self::plain(key)
        }
    }

    /// Its parts as they show, one key cap each: "Ctrl", "Shift", "H".
    pub fn caps(&self) -> Vec<String> {
        let mut caps: Vec<String> = Vec::new();
        if self.ctrl {
            caps.push(CTRL.to_owned());
        }
        if self.alt {
            caps.push(ALT.to_owned());
        }
        if self.shift {
            caps.push(SHIFT.to_owned());
        }
        caps.push(shown_key(&self.key));
        caps
    }

    /// As text shows it: "Ctrl+Shift+H", "Alt+↑".
    pub fn shown(&self) -> String {
        self.caps().join(&JOIN.to_string())
    }

    fn is_function_key(&self) -> bool {
        self.key
            .strip_prefix('F')
            .and_then(|number| number.parse::<u8>().ok())
            .is_some_and(|number| (1..=12).contains(&number))
    }
}

/// A key as the page names it, with a single character in lower case.
fn normalized(key: &str) -> String {
    let mut chars = key.chars();
    match (chars.next(), chars.next()) {
        (Some(only), None) => only.to_lowercase().collect(),
        _ => key.to_owned(),
    }
}

fn shown_key(key: &str) -> String {
    if let Some((_, symbol)) = ARROWS.iter().find(|(name, _)| *name == key) {
        return (*symbol).to_owned();
    }
    if key == SPACE {
        return SPACE_NAME.to_owned();
    }
    let mut chars = key.chars();
    match (chars.next(), chars.next()) {
        (Some(only), None) => only.to_uppercase().collect(),
        _ => key.to_owned(),
    }
}

/// As the preferences file keeps it: "Ctrl+Shift+h", "Alt+ArrowUp", "Space" for the space bar.
impl fmt::Display for Chord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.ctrl {
            write!(formatter, "{CTRL}{JOIN}")?;
        }
        if self.alt {
            write!(formatter, "{ALT}{JOIN}")?;
        }
        if self.shift {
            write!(formatter, "{SHIFT}{JOIN}")?;
        }
        if self.key == SPACE {
            write!(formatter, "{SPACE_NAME}")
        } else {
            write!(formatter, "{}", self.key)
        }
    }
}

/// A chord's text did not read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BadChord;

impl FromStr for Chord {
    type Err = BadChord;

    fn from_str(text: &str) -> Result<Self, BadChord> {
        let mut chord = Self::plain("");
        let mut rest = text;
        while let Some((part, after)) = rest.split_once(JOIN).filter(|(part, _)| !part.is_empty()) {
            match part {
                CTRL => chord.ctrl = true,
                ALT => chord.alt = true,
                SHIFT => chord.shift = true,
                _ => break,
            }
            rest = after;
        }
        let key = if rest == SPACE_NAME { SPACE } else { rest };
        Self::of(key, chord.ctrl, chord.alt, chord.shift).ok_or(BadChord)
    }
}

/// Why a chord cannot run a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// It has no Ctrl or Alt and is no function key: it would take a key typed in a field.
    TypesText,
    /// The app or the system gives it a meaning of its own.
    Reserved(&'static str),
    /// Another command has it.
    Taken(Command),
}

impl Refusal {
    /// What the dialog says, for `chord`.
    pub fn message(&self, chord: &Chord) -> String {
        let shown = chord.shown();
        match self {
            Self::TypesText => {
                format!("{shown} types text. Add Ctrl or Alt, or use F1 to F12.")
            }
            Self::Reserved(meaning) => format!("{shown} {meaning}, and keeps that meaning."),
            Self::Taken(other) => {
                format!("{shown} already runs \u{201c}{}\u{201d}.", other.label())
            }
        }
    }
}

/// The chords with a meaning the app or the system keeps, with that meaning.
fn reserved(chord: &Chord) -> Option<&'static str> {
    let key = chord.key.as_str();
    if key == ESCAPE || key == TAB {
        return Some("moves the focus or closes what is open");
    }
    let only_ctrl = chord.ctrl && !chord.alt;
    let only_alt = chord.alt && !chord.ctrl && !chord.shift;
    match key {
        "z" | "y" if only_ctrl => Some("undoes or redoes typing"),
        "c" | "v" | "x" | "a" if only_ctrl && !chord.shift => {
            Some("copies, pastes or selects text")
        }
        "w" if only_ctrl && !chord.shift => Some("closes the shown file"),
        "r" if only_ctrl && !chord.shift => Some("replies to the selected text"),
        "0" if only_ctrl && !chord.shift => Some("fits a zoomed image"),
        "Enter" if only_alt => Some("sends the message when the turn ends"),
        "F4" if only_alt => Some("closes the window"),
        _ => None,
    }
}

/// The shortcuts of the commands: their defaults, and the ones the Settings dialog changed, which
/// the preferences keep.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Keymap {
    changed: BTreeMap<Command, Chord>,
}

impl Keymap {
    pub fn chord(&self, command: Command) -> Chord {
        self.changed
            .get(&command)
            .cloned()
            .unwrap_or_else(|| command.default_chord())
    }

    /// The command's shortcut as text shows it, such as "Ctrl+K".
    pub fn shown(&self, command: Command) -> String {
        self.chord(command).shown()
    }

    pub fn is_changed(&self, command: Command) -> bool {
        self.changed.contains_key(&command)
    }

    pub fn changed_count(&self) -> usize {
        self.changed.len()
    }

    /// The command `chord` runs.
    pub fn command(&self, chord: &Chord) -> Option<Command> {
        Command::VARIANTS
            .iter()
            .copied()
            .find(|&command| self.chord(command) == *chord)
    }

    /// Whether `chord` can run `command`.
    pub fn check(&self, command: Command, chord: &Chord) -> Result<(), Refusal> {
        if !chord.ctrl && !chord.alt && !chord.is_function_key() {
            return Err(Refusal::TypesText);
        }
        if let Some(meaning) = reserved(chord) {
            return Err(Refusal::Reserved(meaning));
        }
        match self.command(chord) {
            Some(other) if other != command => Err(Refusal::Taken(other)),
            Some(_) | None => Ok(()),
        }
    }

    /// Gives `command` the chord, when it can run it.
    pub fn set(&mut self, command: Command, chord: Chord) -> Result<(), Refusal> {
        self.check(command, &chord)?;
        self.put(command, chord);
        Ok(())
    }

    /// Gives `command` the chord another command has, and that command `command`'s chord.
    pub fn swap(&mut self, command: Command, chord: Chord) -> Result<(), Refusal> {
        let other = match self.check(command, &chord) {
            Ok(()) => return self.set(command, chord),
            Err(Refusal::Taken(other)) => other,
            Err(refusal) => return Err(refusal),
        };
        let given_up = self.chord(command);
        self.put(command, chord);
        self.put(other, given_up);
        Ok(())
    }

    pub fn reset(&mut self, command: Command) -> Result<(), Refusal> {
        self.set(command, command.default_chord())
    }

    pub fn reset_all(&mut self) {
        self.changed.clear();
    }

    fn put(&mut self, command: Command, chord: Chord) {
        if chord == command.default_chord() {
            self.changed.remove(&command);
        } else {
            self.changed.insert(command, chord);
        }
    }

    /// The map with only the changes that hold: a chord no command can take, or that two commands
    /// share, goes back to the default.
    fn checked(changes: BTreeMap<Command, Chord>) -> Self {
        let mut keymap = Self::default();
        for (command, chord) in changes {
            // A refused change keeps the default.
            let _ = keymap.set(command, chord);
        }
        keymap
    }
}

/// The changed shortcuts, by command: `{"switcher": "Ctrl+j"}`.
impl Serialize for Keymap {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let words: BTreeMap<&'static str, String> = self
            .changed
            .iter()
            .map(|(&command, chord)| (command.into(), chord.to_string()))
            .collect();
        words.serialize(serializer)
    }
}

/// A command or a chord this version does not read, from another version of the app or a hand
/// edit, is left out: its command keeps its default.
impl<'de> Deserialize<'de> for Keymap {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let words = BTreeMap::<String, String>::deserialize(deserializer)?;
        let changes = words
            .iter()
            .filter_map(|(command, chord)| Some((command.parse().ok()?, chord.parse().ok()?)))
            .collect();
        Ok(Self::checked(changes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chord(text: &str) -> Chord {
        text.parse().unwrap()
    }

    #[test]
    fn the_command_names_are_pinned() {
        let names: Vec<&str> = Command::VARIANTS
            .iter()
            .map(|&command| command.into())
            .collect();

        assert_eq!(
            names,
            [
                "switcher",
                "new_agent",
                "open_file",
                "files_pane",
                "agent_map",
                "rename",
                "mark_to_handle",
                "close_agent",
                "move_up",
                "move_down",
                "reload_file",
            ]
        );
    }

    #[test]
    fn a_chord_reads_back_as_it_writes_and_shows_its_caps() {
        let texts = ["Ctrl+Shift+h", "Alt+ArrowUp", "F2", "Ctrl+Space", "Ctrl++"];

        for text in texts {
            assert_eq!(chord(text).to_string(), text);
        }
        assert_eq!(chord("Ctrl+Shift+h").shown(), "Ctrl+Shift+H");
        assert_eq!(chord("Alt+ArrowUp").caps(), ["Alt", "↑"]);
        assert_eq!(chord("Ctrl++").key, "+");
        assert_eq!("Ctrl+".parse::<Chord>(), Err(BadChord));
    }

    #[test]
    fn a_press_reads_as_its_chord_and_a_modifier_alone_as_none() {
        assert_eq!(
            Chord::of("K", true, false, true),
            Some(chord("Ctrl+Shift+k"))
        );
        assert_eq!(Chord::of("Control", true, false, false), None);
        assert_eq!(
            Chord::of("ArrowUp", false, true, false),
            Some(chord("Alt+ArrowUp"))
        );
    }

    #[test]
    fn the_defaults_run_their_commands() {
        let keymap = Keymap::default();

        for &command in Command::VARIANTS {
            assert_eq!(keymap.command(&command.default_chord()), Some(command));
            assert_eq!(keymap.check(command, &command.default_chord()), Ok(()));
        }
        assert_eq!(keymap.shown(Command::MoveUp), "Alt+↑");
    }

    #[test]
    fn a_chord_that_types_text_or_has_a_meaning_is_refused() {
        let keymap = Keymap::default();
        let check = |text: &str| keymap.check(Command::Switcher, &chord(text));

        assert_eq!(check("j"), Err(Refusal::TypesText));
        assert_eq!(check("Shift+j"), Err(Refusal::TypesText));
        assert_eq!(check("Enter"), Err(Refusal::TypesText));
        assert!(matches!(check("Ctrl+z"), Err(Refusal::Reserved(_))));
        assert!(matches!(check("Ctrl+w"), Err(Refusal::Reserved(_))));
        assert!(matches!(check("Alt+Enter"), Err(Refusal::Reserved(_))));
        assert!(matches!(check("Ctrl+Escape"), Err(Refusal::Reserved(_))));
        assert_eq!(check("Ctrl+n"), Err(Refusal::Taken(Command::NewAgent)));
        assert_eq!(
            check("Ctrl+Shift+w"),
            Err(Refusal::Taken(Command::CloseAgent))
        );
        assert_eq!(check("Ctrl+j"), Ok(()));
        assert_eq!(check("F7"), Ok(()));
        assert_eq!(
            check("Ctrl+Shift+z"),
            Err(Refusal::Reserved("undoes or redoes typing"))
        );
    }

    #[test]
    fn a_change_holds_and_a_default_again_is_no_change() {
        let mut keymap = Keymap::default();

        keymap.set(Command::Switcher, chord("Ctrl+j")).unwrap();
        let changed = (
            keymap.command(&chord("Ctrl+j")),
            keymap.command(&chord("Ctrl+k")),
        );
        keymap.set(Command::Switcher, chord("Ctrl+k")).unwrap();

        assert_eq!(changed, (Some(Command::Switcher), None));
        assert_eq!(keymap.changed_count(), 0);
        assert!(!keymap.is_changed(Command::Switcher));
    }

    #[test]
    fn a_swap_gives_the_other_command_the_chord_given_up() {
        let mut keymap = Keymap::default();

        keymap.swap(Command::CloseAgent, chord("Ctrl+k")).unwrap();

        assert_eq!(keymap.shown(Command::CloseAgent), "Ctrl+K");
        assert_eq!(keymap.shown(Command::Switcher), "Ctrl+Shift+W");
        assert_eq!(keymap.changed_count(), 2);
    }

    #[test]
    fn a_reset_is_refused_while_another_command_holds_the_default() {
        let mut keymap = Keymap::default();
        keymap.set(Command::Switcher, chord("Ctrl+j")).unwrap();
        keymap.set(Command::NewAgent, chord("Ctrl+k")).unwrap();

        let refused = keymap.reset(Command::Switcher);
        keymap.reset_all();

        assert_eq!(refused, Err(Refusal::Taken(Command::NewAgent)));
        assert_eq!(keymap, Keymap::default());
    }

    #[test]
    fn the_preferences_keep_the_changes_and_drop_what_does_not_hold() {
        let mut keymap = Keymap::default();
        keymap.set(Command::Switcher, chord("Ctrl+j")).unwrap();
        let written = serde_json::to_value(&keymap).unwrap();
        let read: Keymap = serde_json::from_value(serde_json::json!({
            "switcher": "Ctrl+j",
            "new_agent": "Ctrl+j",
            "open_file": "x",
            "teleport": "Ctrl+t",
            "agent_map": "Ctrl+",
        }))
        .unwrap();

        assert_eq!(written, serde_json::json!({ "switcher": "Ctrl+j" }));
        assert_eq!(read, keymap);
    }
}
